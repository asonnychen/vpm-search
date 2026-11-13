//! sing-box 进程的启动、就绪等待、经本地入站测一次延迟，以及退出清理。

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::geo::{self, ExitIp};
use crate::logs::SingboxLog;
use crate::template::NodeTemplate;

use super::config::build_config;
use super::port::pick_free_port;
use super::speed::{self, SpeedOptions};
use super::{classify, Attempt, Endpoint, ProbeOptions, ProbeOutcome, Prober, PROBE_URL};

/// 启动 sing-box 后等待本地入站就绪的上限。
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

/// 一份临时配置文件，离开作用域即删除。
pub struct TempConfig {
    path: PathBuf,
}

impl TempConfig {
    pub fn create(dir: &Path, name: &str, contents: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(name);
        std::fs::write(&path, contents)?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// 从候选路径里挑第一个存在的。
pub fn pick_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.is_file()).cloned()
}

/// 真探测器：每条地址起一个 sing-box 进程。
pub struct SingBoxProber {
    binary: PathBuf,
    log: Arc<SingboxLog>,
    log_path: PathBuf,
    temp_dir: PathBuf,
}

impl SingBoxProber {
    pub fn new(
        binary: impl Into<PathBuf>,
        log: Arc<SingboxLog>,
        log_path: impl Into<PathBuf>,
        temp_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            binary: binary.into(),
            log,
            log_path: log_path.into(),
            temp_dir: temp_dir.into(),
        }
    }

    fn log_hint(&self) -> String {
        format!("sing-box 日志：{}", self.log_path.display())
    }

    /// 起进程、等就绪、请求 204 端点，返回原始观测。
    fn attempt(
        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> Attempt {
        if !self.binary.is_file() {
            return Attempt::connect_failed(format!(
                "未找到 sing-box（{}），请先运行 scripts/fetch-singbox.ps1。{}",
                self.binary.display(),
                self.log_hint()
            ));
        }

        // 端口被占就顺延到下一个，直到区间末尾。
        let Some(port) = pick_free_port() else {
            return Attempt::connect_failed(format!(
                "20000–29999 内没有可用端口。{}",
                self.log_hint()
            ));
        };

        let config = build_config(endpoint, template, port);
        let contents = serde_json::to_string_pretty(&config).unwrap_or_default();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("probe-{}-{unique}.json", std::process::id());
        let config = match TempConfig::create(&self.temp_dir, &name, &contents) {
            Ok(config) => config,
            Err(error) => {
                return Attempt::connect_failed(format!(
                    "写入临时配置失败：{error}。{}",
                    self.log_hint()
                ))
            }
        };

        let mut session = match self.spawn(config) {
            Ok(session) => session,
            Err(error) => return Attempt::connect_failed(error),
        };

        if let Err(error) = wait_until_ready(&mut session.child, port, STARTUP_TIMEOUT) {
            return Attempt::connect_failed(format!("{error}。{}", self.log_hint()));
        }

        // 只经本地 mixed 入站出去，不读取系统代理（ADR-0004）。
        let client = match reqwest::blocking::Client::builder()
            .proxy(match reqwest::Proxy::all(format!("http://127.0.0.1:{port}")) {
                Ok(proxy) => proxy,
                Err(error) => {
                    return Attempt::connect_failed(format!("本地代理地址无效：{error}"))
                }
            })
            .connect_timeout(options.connect_timeout)
            .timeout(options.request_timeout)
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                return Attempt::connect_failed(format!("创建探测客户端失败：{error}"))
            }
        };

        let started = Instant::now();
        let result = client.get(PROBE_URL).send();
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match result {
            Ok(response) if response.status() == reqwest::StatusCode::NO_CONTENT => {
                Attempt::responded(elapsed_ms)
            }
            Ok(response) => Attempt::connect_failed(format!(
                "204 端点返回 HTTP {}",
                response.status().as_u16()
            )),
            Err(error) => {
                // 进程中途崩溃时，先把日志路径告诉用户。
                if let Ok(Some(status)) = session.child.try_wait() {
                    return Attempt::connect_failed(format!(
                        "sing-box 中途退出（{status}），本轮已终止。{}",
                        self.log_hint()
                    ));
                }
                if error.is_timeout() {
                    Attempt::no_response(format!(
                        "{} 秒内未收到响应（{}）",
                        options.request_timeout.as_secs(),
                        error
                    ))
                } else {
                    Attempt::connect_failed(format!("连接阶段失败：{error}"))
                }
            }
        }
    }

    fn spawn(&self, config: TempConfig) -> Result<Session, String> {
        let child = spawn_singbox(&self.binary, config.path(), &self.log).map_err(|error| {
            format!(
                "启动 sing-box 失败：{error}。{}",
                self.log_hint()
            )
        })?;

        Ok(Session {
            child,
            _config: config,
        })
    }

    /// 为一条地址起一个临时 sing-box 进程并等本地入站就绪，返回会话与端口。
    fn start_session(
        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
    ) -> Result<(Session, u16), String> {
        if !self.binary.is_file() {
            return Err(format!(
                "未找到 sing-box（{}），请先运行 scripts/fetch-singbox.ps1。{}",
                self.binary.display(),
                self.log_hint()
            ));
        }

        let port = pick_free_port().ok_or_else(|| {
            format!("20000–29999 内没有可用端口。{}", self.log_hint())
        })?;

        let config = build_config(endpoint, template, port);
        let contents = serde_json::to_string_pretty(&config).unwrap_or_default();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("probe-{}-{unique}.json", std::process::id());
        let config = TempConfig::create(&self.temp_dir, &name, &contents)
            .map_err(|error| format!("写入临时配置失败：{error}。{}", self.log_hint()))?;

        let mut session = self.spawn(config)?;
        wait_until_ready(&mut session.child, port, STARTUP_TIMEOUT)
            .map_err(|error| format!("{error}。{}", self.log_hint()))?;
        Ok((session, port))
    }
}

/// 启动一个 sing-box 进程，并把 stdout/stderr 推进日志通道。
pub(crate) fn spawn_singbox(
    binary: &Path,
    config_path: &Path,
    log: &Arc<SingboxLog>,
) -> std::io::Result<Child> {
    let mut command = Command::new(binary);
    command
        .arg("run")
        .arg("-c")
        .arg(config_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Windows 下不要弹出控制台窗口。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command.spawn()?;

    // 子进程输出推进 sing-box 日志通道。
    if let Some(stdout) = child.stdout.take() {
        let log = log.clone();
        std::thread::spawn(move || pump("stdout", stdout, log));
    }
    if let Some(stderr) = child.stderr.take() {
        let log = log.clone();
        std::thread::spawn(move || pump("stderr", stderr, log));
    }

    Ok(child)
}

impl Prober for SingBoxProber {
    fn measure_latency(
        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> ProbeOutcome {
        classify(self.attempt(endpoint, template, options))
    }

    /// 取出口 IP：为这条地址起临时链路，经本地入站请求两个端点并取多数。
    fn exit_ip(
        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> Result<ExitIp, String> {
        let (mut session, port) = self.start_session(endpoint, template)?;
        let cancel = AtomicBool::new(false);
        let result = geo::fetch_exit_ip(
            Some(&format!("http://127.0.0.1:{port}")),
            options.connect_timeout,
            options.request_timeout,
            &cancel,
        );

        if result.is_err() {
            if let Ok(Some(status)) = session.child.try_wait() {
                return Err(format!(
                    "sing-box 中途退出（{status}），本轮已终止。{}",
                    self.log_hint()
                ));
            }
        }
        result
    }

    /// 单地址测速：同一条代理链路上并发下载 4×2.5MB，上限 15 秒。
    fn measure_speed(        &self,
        endpoint: &Endpoint,
        template: &NodeTemplate,
        options: &ProbeOptions,
    ) -> Result<f64, String> {
        let (mut session, port) = self.start_session(endpoint, template)?;
        let speed = SpeedOptions::default();
        let cancel = AtomicBool::new(false);
        let result = speed::download_streams(
            Some(&format!("http://127.0.0.1:{port}")),
            options.connect_timeout,
            &speed,
            &cancel,
        );

        if result.is_err() {
            if let Ok(Some(status)) = session.child.try_wait() {
                return Err(format!(
                    "sing-box 中途退出（{status}），本轮已终止。{}",
                    self.log_hint()
                ));
            }
        }
        result
    }
}

/// 一次探测的进程与配置；Drop 时先杀进程再删配置。
struct Session {
    child: Child,
    _config: TempConfig,
}

impl Session {
    /// 终止子进程；返回退出状态供测试断言。
    fn stop(&mut self) -> Option<std::process::ExitStatus> {
        let _ = self.child.kill();
        self.child.wait().ok()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// 把子进程某个流的每一行推进日志通道。
fn pump(stream: &'static str, pipe: impl std::io::Read, log: Arc<SingboxLog>) {
    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
        log.push(stream, line);
    }
}

/// 等本地入站端口可连接；期间进程退出则立刻失败。
pub(crate) fn wait_until_ready(
    child: &mut Child,
    port: u16,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("sing-box 启动失败（{status}）"));
        }
        if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!(
        "sing-box 未在 {} 秒内监听 127.0.0.1:{port}",
        timeout.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 临时配置在_drop_后删除() {
        let dir = tempfile::tempdir().unwrap();
        let path = {
            let config = TempConfig::create(dir.path(), "probe.json", "{}").unwrap();
            let path = config.path().to_path_buf();
            assert!(path.exists());
            path
        };
        assert!(!path.exists());
        // 目录本身保留，交给系统清理。
        assert!(dir.path().exists());
    }

    #[test]
    fn 候选二进制里挑第一个存在的() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.exe");
        let present = dir.path().join("sing-box.exe");
        std::fs::write(&present, b"stub").unwrap();

        assert_eq!(
            pick_existing(&[missing.clone(), present.clone()]),
            Some(present)
        );
        assert_eq!(pick_existing(&[missing]), None);
    }

    #[test]
    fn 二进制不存在时失败原因带日志路径() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(SingboxLog::spawn(Arc::new(crate::logs::LogWriter::new(
            dir.path(),
        ))));
        let log_path = dir.path().join("singbox.log");
        let prober = SingBoxProber::new(
            dir.path().join("nope.exe"),
            log.clone(),
            log_path.clone(),
            dir.path().join("tmp"),
        );

        let outcome = prober.measure_latency(
            &Endpoint::new("104.16.1.1", 8443),
            &NodeTemplate::default(),
            &ProbeOptions::default(),
        );
        assert_eq!(outcome.status, super::super::ProbeStatus::Fail);
        assert!(outcome.error.unwrap().contains(&log_path.display().to_string()));
        log.shutdown();
    }

    #[test]
    fn 会话结束时进程被终止且临时配置被删除() {
        let dir = tempfile::tempdir().unwrap();
        let config = TempConfig::create(dir.path(), "probe.json", "{}").unwrap();
        let config_path = config.path().to_path_buf();

        let mut command = if cfg!(windows) {
            let mut command = std::process::Command::new("cmd");
            command.args(["/c", "ping", "-n", "30", "127.0.0.1"]);
            command
        } else {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            command
        };
        let child = command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let mut session = Session {
            child,
            _config: config,
        };
        assert!(session.stop().is_some(), "进程应被终止并回收");
        assert!(config_path.exists(), "删除发生在 Drop 里");
        drop(session);
        assert!(!config_path.exists());
    }

    #[test]
    fn 就绪等待能识别进程提前退出() {        // 用系统里必然存在的命令跑一个立即退出的进程。
        let mut child = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/c", "exit", "3"])
                .spawn()
                .unwrap()
        } else {
            std::process::Command::new("true").spawn().unwrap()
        };
        let result = wait_until_ready(&mut child, 9, Duration::from_secs(2));
        assert!(result.is_err());
        let _ = child.wait();
    }
}
