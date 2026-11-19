#![cfg_attr(windows, windows_subsystem = "windows")]

mod phone;
#[cfg(windows)]
mod tray;
use anyhow::{Context, Result, bail, ensure};
use idevice::{IdeviceService, heartbeat::HeartbeatClient};
use phone::{Phone, revision};
use std::{env, fs, net::IpAddr, path::PathBuf, time::Duration};
use tokio::time::{sleep, timeout};

struct Options {
    host: Option<IpAddr>,
    device: Option<String>,
    check: bool,
    prepare: Option<PathBuf>,
    receive_current: bool,
}

impl Options {
    fn parse() -> Result<Self> {
        let mut options = Self {
            host: None,
            device: None,
            check: false,
            prepare: None,
            receive_current: false,
        };
        let mut usb = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--host" => {
                    options.host = Some(args.next().context("--host needs an IP address")?.parse()?)
                }
                "--device" => options.device = Some(args.next().context("--device needs a UDID")?),
                "--prepare-ddi" => {
                    options.prepare = Some(
                        args.next()
                            .context("--prepare-ddi needs a Restore directory")?
                            .into(),
                    );
                    options.check = true;
                }
                "--check" => options.check = true,
                "--usb" => usb = true,
                "--receive-current" => options.receive_current = true,
                _ => bail!("unrecognized option {arg}"),
            }
        }
        ensure!(!(usb && options.host.is_some()), "choose USB or Wi-Fi");
        ensure!(
            options.prepare.is_none() || options.host.is_none(),
            "prepare the device over USB"
        );
        Ok(options)
    }
}

fn status(message: &str) {
    eprintln!("{message}");
    if let Some(dir) = env::var_os("LOCALAPPDATA") {
        let dir = PathBuf::from(dir).join("ClipboardNative");
        if fs::create_dir_all(&dir).is_ok() {
            let _ = fs::write(dir.join("status.txt"), message);
        }
    }
}

#[cfg(windows)]
mod desktop {
    use anyhow::{Result, ensure};
    use clipboard_win::{Clipboard, formats, raw};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
        System::Threading::CreateMutexW,
    };
    pub fn sequence() -> u32 {
        raw::seq_num().map_or(0, |v| v.get())
    }
    pub fn read() -> Option<(u32, Option<String>)> {
        let _lock = Clipboard::new_attempts(8).ok()?;
        let text = if raw::is_format_avail(formats::CF_UNICODETEXT) {
            Some(clipboard_win::get(formats::Unicode).ok()?)
        } else {
            None
        };
        Some((sequence(), text))
    }
    pub fn write(expected: u32, text: &str) -> Option<u32> {
        let _lock = Clipboard::new_attempts(8).ok()?;
        if sequence() != expected {
            return None;
        }
        clipboard_win::set(formats::Unicode, text).ok()?;
        Some(sequence())
    }
    pub struct Instance(HANDLE);
    impl Instance {
        pub fn acquire() -> Result<Self> {
            let name: Vec<u16> = "Local\\iCloudCopyBoard.UserClipboard"
                .encode_utf16()
                .chain([0])
                .collect();
            // The named handle exists for the lifetime of this process in this logon session.
            let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            ensure!(!h.is_null(), "cannot create instance mutex");
            let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            let guard = Self(h);
            ensure!(!existing, "clipboard service is already running");
            Ok(guard)
        }
    }
    impl Drop for Instance {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(windows)]
struct State {
    seq: u32,
    revision: Option<phone::Revision>,
    receive_current: bool,
    tooltip: tokio::sync::watch::Sender<&'static str>,
}

#[cfg(windows)]
async fn sync(phone: &mut Phone, state: &mut State) -> Result<()> {
    if state.revision.is_none() && !state.receive_current {
        state.revision = Some(revision(&phone.pull(true).await?)?);
    }
    status("Connected");
    state.tooltip.send_replace("iPhone 剪贴板共享 · 已连接");
    let mut poll = tokio::time::Instant::now();
    loop {
        if desktop::sequence() != state.seq
            && let Some((seq, text)) = desktop::read()
        {
            if let Some(text) = text.filter(|s| phone::valid(s)) {
                state.revision = Some(revision(&phone.set(&text).await?)?);
            }
            state.seq = seq;
        }
        if tokio::time::Instant::now() >= poll {
            let latest = revision(&phone.pull(true).await?)?;
            if Some(&latest) != state.revision.as_ref() {
                let snapshot = phone.pull(false).await?;
                let current = revision(&snapshot)?;
                let text = timeout(Duration::from_secs(20), phone.text(&snapshot)).await??;
                if revision(&phone.pull(true).await?)? == current
                    && desktop::sequence() == state.seq
                {
                    match text {
                        phone::TextUpdate::Text(text) => {
                            if let Some(seq) = desktop::write(state.seq, &text) {
                                state.seq = seq;
                                state.revision = Some(current);
                            }
                        }
                        phone::TextUpdate::Unsupported => state.revision = Some(current),
                        phone::TextUpdate::Retry => (),
                    }
                }
            }
            poll = tokio::time::Instant::now() + Duration::from_secs(1);
        }
        sleep(Duration::from_millis(160)).await;
    }
}

#[cfg(windows)]
async fn session(options: &Options, state: &mut State) -> Result<()> {
    let provider = timeout(
        Duration::from_secs(12),
        phone::provider(options.host, options.device.as_deref()),
    )
    .await??;
    let hb = timeout(
        Duration::from_secs(15),
        HeartbeatClient::connect(&*provider),
    )
    .await
    .context("heartbeat timeout")??;
    // Keep the heartbeat live while opening RSD and while the device is idle.
    let transfer = async {
        let seconds = if options.prepare.is_some() { 180 } else { 20 };
        let connect = Phone::connect(&*provider, options.prepare.as_deref());
        let mut phone = timeout(Duration::from_secs(seconds), connect)
            .await
            .context("connection timed out")??;
        if options.check {
            revision(&phone.pull(true).await?)?;
            println!("Paired clipboard service verified");
            Ok(())
        } else {
            sync(&mut phone, state).await
        }
    };
    tokio::select! {
        result = phone::heartbeat(hb) => result,
        result = transfer => result,
    }
}

#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let result: Result<()> = async {
        let options = Options::parse()?;
        let _guard = if options.check {
            None
        } else {
            Some(desktop::Instance::acquire()?)
        };
        let (tooltip, updates) = tokio::sync::watch::channel("iPhone 剪贴板共享 · 正在连接");
        let mut state = State {
            seq: desktop::sequence(),
            revision: None,
            receive_current: options.receive_current,
            tooltip,
        };
        if options.check {
            return session(&options, &mut state).await;
        }
        let service = async {
            loop {
                if let Err(e) = session(&options, &mut state).await {
                    status(&format!("Reconnecting: {e}"));
                }
                state.tooltip.send_replace("iPhone 剪贴板共享 · 正在重连");
                sleep(Duration::from_secs(3)).await;
            }
        };
        tokio::select! {
            result = service => result,
            result = tray::run(updates) => result,
        }
    }
    .await;
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Run this clipboard service on Windows.");
    std::process::exit(1)
}
