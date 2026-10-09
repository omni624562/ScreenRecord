//! 執行子行程。
//! Windows 上不顯示主控台視窗；逾時會強制結束。

use std::ffi::OsStr;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// 建立不顯示主控台視窗的指令
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c.kill_on_drop(true);
    c
}

/// 同步版（std::process）：不顯示主控台視窗。用於在一般執行緒上讀取輸出（播放器）
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// 執行並收集輸出（逾時強制結束）
pub async fn run<S: AsRef<OsStr>>(program: impl AsRef<OsStr>, args: &[S], timeout: Duration) -> RunResult {
    let mut cmd = command(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunResult { code: -1, stderr: e.to_string(), ..Default::default() },
    };
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => {
            RunResult { code: out.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr: String::from_utf8_lossy(&out.stderr).into_owned(), timed_out: false }
        }
        Ok(Err(e)) => RunResult { code: -1, stderr: e.to_string(), ..Default::default() },
        // 逾時：wait_with_output 的 future 被丟棄時，kill_on_drop 會結束子行程
        Err(_) => RunResult { code: -1, timed_out: true, ..Default::default() },
    }
}

/// 逐行讀取（去掉結尾的 \r）；串流結束才回傳
pub async fn read_lines<R: AsyncRead + Unpin>(stream: R, mut on_line: impl FnMut(&str)) {
    let mut lines = BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        on_line(line.trim_end_matches('\r'));
    }
}

/// 最後幾行（去掉空白行），以「 / 」連接：錯誤訊息用
pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_lines_picks_tail() {
        assert_eq!(last_lines("a\n\n b \r\nc\nd\n", 3), "b / c / d");
        assert_eq!(last_lines("", 3), "");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_collects_output_and_times_out() {
        let r = run("sh", &["-c", "echo out; echo err >&2; exit 3"], Duration::from_secs(5)).await;
        assert_eq!((r.code, r.stdout.trim(), r.stderr.trim(), r.timed_out), (3, "out", "err", false));
        let t = run("sh", &["-c", "sleep 5"], Duration::from_millis(100)).await;
        assert!(t.timed_out);
        let missing = run("definitely-not-a-program", &[] as &[&str], Duration::from_secs(1)).await;
        assert_eq!(missing.code, -1);
    }
}
