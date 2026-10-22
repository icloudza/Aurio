import AppKit
import Sparkle

/// 在线更新（Sparkle 2）。
///
/// 更新清单 appcast.xml 和安装盘都放在 GitHub Release。为了国内网络：
/// - 检查前先依次探测国内加速镜像，第一个能拿到清单的就用它，都不行再用 GitHub 官方；
/// - 下载前把安装盘地址改成同一个来源；
/// - 清单或下载出错时，把这个镜像标记为暂时不可用，换下一个来源自动重试。
/// Sparkle 用 EdDSA 签名校验安装盘（公钥是 Info.plist 的 SUPublicEDKey），镜像改了内容会被拒绝。
///
/// 菜单栏 App 平时不在前台，所以后台检查到新版本时不直接弹窗，只在菜单栏图标和面板上提示（温和提醒）。
final class UpdateController: NSObject, ObservableObject, SPUUpdaterDelegate, SPUStandardUserDriverDelegate {
    /// 和 crates/aurio-update 的 DEFAULT_MIRRORS 保持一致
    static let mirrors = ["https://gh-proxy.com/", "https://gh.llkk.cc/", "https://ghfast.top/"]
    static let appcastURL = "https://github.com/icloudza/Aurio/releases/latest/download/appcast.xml"

    /// 后台检查发现的新版本（面板和菜单栏图标据此提示）
    @Published private(set) var availableVersion: String?
    /// 正在探测镜像（面板上的「检查更新」显示为进行中）
    @Published private(set) var probing = false

    private var controller: SPUStandardUpdaterController?
    /// 当前来源前缀；空串表示 GitHub 官方
    private var prefix = ""
    private var lastProbe = Date.distantPast
    /// 最近失败过的镜像，6 小时内不再用
    private var failedAt: [String: Date] = [:]
    /// 本轮检查已经换过几次来源（防止无限重试）
    private var retries = 0
    private var pendingRetry: SPUUpdateCheck?

    var canCheck: Bool { controller?.updater.canCheckForUpdates ?? false }

    /// 只给界面截图用：假装后台发现了新版本
    func showPreview(version: String?) {
        availableVersion = version
    }

    /// start 为 false 时不启动 Sparkle、不联网（截图和测试模式）
    init(start: Bool) {
        super.init()
        guard start else { return }
        let c = SPUStandardUpdaterController(startingUpdater: false, updaterDelegate: self, userDriverDelegate: self)
        controller = c
        // 先选好来源再启动（启动后 Sparkle 会按计划在后台检查）
        probe { c.startUpdater() }
    }

    /// 用户点「检查更新」
    func checkForUpdates() {
        guard let c = controller, c.updater.canCheckForUpdates else { return }
        NSApp.activate(ignoringOtherApps: true)
        retries = 0
        let run = { c.checkForUpdates(nil) }
        // 10 分钟内探测过、且当前来源没失败过，就直接用
        if Date().timeIntervalSince(lastProbe) < 600 { run() } else { probe(run) }
    }

    // MARK: - 选择来源

    private var candidates: [String] {
        let now = Date()
        return Self.mirrors.filter { m in failedAt[m].map { now.timeIntervalSince($0) > 6 * 3600 } ?? true } + [""]
    }

    /// 依次请求各来源的 appcast.xml（6 秒超时），第一个返回有效清单的作为当前来源
    private func probe(_ done: @escaping () -> Void) {
        probing = true
        let list = candidates
        let session = URLSession(configuration: {
            let c = URLSessionConfiguration.ephemeral
            c.timeoutIntervalForRequest = 6
            c.requestCachePolicy = .reloadIgnoringLocalCacheData
            return c
        }())
        func attempt(_ i: Int) {
            guard i < list.count else {
                finish("")   // 全失败：交给 Sparkle 用官方地址，它会报告错误
                return
            }
            guard let url = URL(string: list[i] + Self.appcastURL) else { return attempt(i + 1) }
            session.dataTask(with: url) { data, resp, _ in
                let ok = (resp as? HTTPURLResponse)?.statusCode == 200
                    && data.map { String(decoding: $0.prefix(512), as: UTF8.self).contains("<rss") } == true
                if ok { finish(list[i]) } else { attempt(i + 1) }
            }.resume()
        }
        func finish(_ p: String) {
            DispatchQueue.main.async {
                self.prefix = p
                self.lastProbe = Date()
                self.probing = false
                NSLog("Aurio: 更新来源 %@", p.isEmpty ? "GitHub 官方" : p)
                done()
            }
        }
        attempt(0)
    }

    // MARK: - SPUUpdaterDelegate

    func feedURLString(for updater: SPUUpdater) -> String? {
        prefix + Self.appcastURL
    }

    func updater(_ updater: SPUUpdater, willDownloadUpdate item: SUAppcastItem, with request: NSMutableURLRequest) {
        guard !prefix.isEmpty, let original = request.url?.absoluteString,
              original.hasPrefix("https://github.com/"), let url = URL(string: prefix + original) else { return }
        request.url = url
    }

    func updater(_ updater: SPUUpdater, didFinishUpdateCycleFor updateCheck: SPUUpdateCheck, error: Error?) {
        guard let e = error as NSError?, e.domain == SUSparkleErrorDomain else { return }
        // 清单 / 下载 / 签名校验出错，且用的是镜像：换下一个来源重试
        let retryable: Set<Int> = [1000, 1002, 2001, 3001, 3002]
        guard retryable.contains(e.code), !prefix.isEmpty, retries < Self.mirrors.count else { return }
        NSLog("Aurio: 镜像 %@ 出错（%d），换下一个来源", prefix, e.code)
        failedAt[prefix] = Date()
        retries += 1
        let check = updateCheck
        probe { [weak self] in
            guard let c = self?.controller else { return }
            if check == .updatesInBackground { c.updater.checkForUpdatesInBackground() } else { c.checkForUpdates(nil) }
        }
    }

    // MARK: - SPUStandardUserDriverDelegate（温和提醒）

    var supportsGentleScheduledUpdateReminders: Bool { true }

    func standardUserDriverShouldHandleShowingScheduledUpdate(_ update: SUAppcastItem, andInImmediateFocus immediateFocus: Bool) -> Bool {
        // 刚启动等用户正在看着的时候直接显示；其余时候只做提示，等用户自己点
        immediateFocus
    }

    func standardUserDriverWillHandleShowingUpdate(_ handleShowingUpdate: Bool, forUpdate update: SUAppcastItem, state: SPUUserUpdateState) {
        if !handleShowingUpdate && !state.userInitiated {
            availableVersion = update.displayVersionString
        }
    }

    func standardUserDriverDidReceiveUserAttention(forUpdate update: SUAppcastItem) {
        availableVersion = nil
    }

    func standardUserDriverWillFinishUpdateSession() {
        availableVersion = nil
    }
}
