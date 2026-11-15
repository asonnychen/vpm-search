fn main() {
    // 自派生：带 `--probe-worker` 启动的是探测子进程，不初始化窗口。
    if std::env::args().any(|arg| arg == "--probe-worker") {
        std::process::exit(vpm_search_lib::run_probe_worker());
    }
    vpm_search_lib::run()
}
