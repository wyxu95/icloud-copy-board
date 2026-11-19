use anyhow::{Context, Result, ensure};
use idevice::{
    IdeviceService, ReadWrite, RemoteXpcClient,
    core_device_proxy::CoreDeviceProxy,
    heartbeat::HeartbeatClient,
    provider::{IdeviceProvider, TcpProvider},
    rsd::RsdHandshake,
    tcp::handle::AdapterHandle,
    usbmuxd::UsbmuxdAddr,
    xpc,
    xpc::XPCObject,
};
use plist::Value;
use std::{env, net::IpAddr, path::PathBuf, time::Duration};
use tokio::time::timeout;

const TYPES: [&str; 3] = ["public.utf8-plain-text", "public.plain-text", "public.text"];
pub const LIMIT: usize = 1024 * 1024;

fn only_record() -> Result<String> {
    let dir = PathBuf::from(env::var_os("ProgramData").context("ProgramData missing")?)
        .join("Apple")
        .join("Lockdown");
    let mut ids = vec![];
    for file in dir
        .read_dir()
        .context("Apple pairing records unavailable")?
    {
        let path = file?.path();
        let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if path.extension().is_some_and(|s| s == "plist")
            && name.len() >= 24
            && name.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
        {
            ids.push(name.to_owned());
        }
    }
    ensure!(
        ids.len() == 1,
        "select a USB-paired device with --device UDID"
    );
    Ok(ids.remove(0))
}

pub async fn provider(host: Option<IpAddr>, id: Option<&str>) -> Result<Box<dyn IdeviceProvider>> {
    let addr = UsbmuxdAddr::default();
    let mut mux = addr
        .connect(1)
        .await
        .context("Apple Mobile Device Service unavailable")?;
    if let Some(host) = host {
        let id = match id {
            Some(s) => s.to_owned(),
            None => only_record()?,
        };
        return Ok(Box::new(TcpProvider {
            addr: host,
            scope_id: None,
            pairing_file: mux.get_pair_record(&id).await?,
            label: "icloud-copy-board".into(),
        }));
    }
    let mut devices = mux.get_devices().await?;
    devices.retain(|d| {
        matches!(d.connection_type, idevice::usbmuxd::Connection::Usb)
            && id.is_none_or(|id| d.udid == id)
    });
    ensure!(
        devices.len() == 1,
        "connect one trusted iPhone over USB or select --device UDID"
    );
    Ok(Box::new(devices[0].to_provider(addr, "icloud-copy-board")))
}

pub async fn heartbeat(mut client: HeartbeatClient) -> Result<()> {
    let mut interval = 60;
    loop {
        interval = client.get_marco(interval).await?.saturating_add(5);
        client.send_polo().await?;
    }
}

pub struct Phone {
    wire: RemoteXpcClient<Box<dyn ReadWrite>>,
    _adapter: AdapterHandle,
}

impl Phone {
    pub async fn connect(
        provider: &dyn IdeviceProvider,
        prepare: Option<&std::path::Path>,
    ) -> Result<Self> {
        if prepare.is_some() {
            let mut lock = idevice::lockdown::LockdownClient::connect(provider).await?;
            lock.start_session(&provider.get_pairing_file().await?)
                .await?;
            lock.set_value(
                "EnableWifiConnections",
                Value::Boolean(true),
                Some("com.apple.mobile.wireless_lockdown"),
            )
            .await?;
        }
        let proxy = CoreDeviceProxy::connect(provider)
            .await
            .context("CoreDevice tunnel")?;
        let port = proxy.tunnel_info().server_rsd_port;
        let mut tunnel = proxy.create_software_tunnel()?;
        tunnel.set_mss(1400);
        let mut adapter = tunnel.to_async_handle();
        let mut rsd = RsdHandshake::new(adapter.connect(port).await?)
            .await
            .context("RSD discovery")?;
        if !rsd
            .services
            .contains_key("com.apple.coredevice.pasteboardservice")
            && let Some(path) = prepare
        {
            let assets = idevice::cryptexd::Cryptex1Assets::load(path)
                .await
                .context("reading the DDI Restore directory")?;
            idevice::cryptexd::install_ddi(&mut adapter, &mut rsd, &assets)
                .await
                .context("installing signed developer image")?;
            rsd = RsdHandshake::new(adapter.connect(port).await?).await?;
        }
        let port = rsd
            .services
            .get("com.apple.coredevice.pasteboardservice")
            .context("enable Developer Mode and mount the developer disk image")?
            .port;
        let socket: Box<dyn ReadWrite> = Box::new(adapter.connect(port).await?);
        let mut wire = RemoteXpcClient::new(socket).await?;
        wire.do_handshake().await?;
        Ok(Self {
            wire,
            _adapter: adapter,
        })
    }

    async fn request(&mut self, msg: XPCObject, expected: &str) -> Result<Value> {
        timeout(Duration::from_secs(20), async {
            self.wire.send_object(msg, true).await?;
            let reply = self.wire.recv().await?;
            ensure!(
                reply
                    .as_dictionary()
                    .and_then(|d| d.get("command"))
                    .and_then(Value::as_string)
                    == Some(expected),
                "device declined {expected} request"
            );
            Ok(reply)
        })
        .await
        .context("pasteboard request timed out")?
    }

    pub async fn pull(&mut self, metadata_only: bool) -> Result<Value> {
        let policy = if metadata_only {
            xpc!({"allPromised": XPCObject::Dictionary(Default::default())})
        } else {
            xpc!({"promiseSecondary": XPCObject::Dictionary(Default::default())})
        };
        let reply = self
            .request(
                xpc!({"command":"PULL","pasteboardName":"general","dataPolicy":policy}),
                "PULL_REPLY",
            )
            .await?;
        board(reply)
    }

    pub async fn set(&mut self, text: &str) -> Result<Value> {
        let mut data = idevice::xpc::Dictionary::new();
        for uti in TYPES {
            data.insert(uti.into(), xpc!({"data":text.as_bytes()}));
        }
        board(
            self.request(
                xpc!({"command":"SET","pasteboardName":"general",
            "items":[{"types":TYPES.to_vec(),"data":XPCObject::Dictionary(data)}],
            "sourceMetadata":XPCObject::Null}),
                "SET_REPLY",
            )
            .await?,
        )
    }

    pub async fn text(&mut self, snapshot: &Value) -> Result<TextUpdate> {
        let mut unavailable = false;
        for entry in text_entries(snapshot) {
            let resolved;
            let bytes = if let Some(bytes) = entry.inline {
                bytes
            } else {
                let message = xpc!({
                    "command": "RESOLVE", "pasteboardName": "general",
                    "itemIndex": entry.index as i64, "type": entry.uti
                });
                self.wire.send_object(message, true).await?;
                resolved = self.wire.recv().await?;
                let fields = resolved.as_dictionary();
                let bytes = fields.and_then(|d| d.get("data")).and_then(Value::as_data);
                if fields
                    .and_then(|d| d.get("command"))
                    .and_then(Value::as_string)
                    != Some("DATA")
                    || bytes.is_none()
                {
                    unavailable = true;
                    continue;
                }
                bytes.unwrap()
            };
            if let Ok(text) = std::str::from_utf8(bytes)
                && valid(text)
            {
                return Ok(TextUpdate::Text(text.to_owned()));
            }
        }
        Ok(if unavailable {
            TextUpdate::Retry
        } else {
            TextUpdate::Unsupported
        })
    }
}

pub enum TextUpdate {
    Text(String),
    Unsupported,
    Retry,
}

struct TextEntry<'a> {
    index: usize,
    uti: &'static str,
    inline: Option<&'a [u8]>,
}

fn text_entries(board: &Value) -> Vec<TextEntry<'_>> {
    let mut entries = Vec::new();
    if let Some(items) = board
        .as_dictionary()
        .and_then(|d| d.get("items"))
        .and_then(Value::as_array)
    {
        for (index, item) in items.iter().enumerate() {
            let Some(fields) = item.as_dictionary() else {
                continue;
            };
            let Some(types) = fields.get("types").and_then(Value::as_array) else {
                continue;
            };
            for uti in TYPES {
                if types.iter().any(|v| v.as_string() == Some(uti)) {
                    let inline = fields
                        .get("data")
                        .and_then(Value::as_dictionary)
                        .and_then(|d| d.get(uti))
                        .and_then(Value::as_dictionary)
                        .and_then(|d| d.get("data"))
                        .and_then(Value::as_data);
                    entries.push(TextEntry { index, uti, inline });
                }
            }
        }
    }
    entries
}

fn board(mut reply: Value) -> Result<Value> {
    reply
        .as_dictionary_mut()
        .and_then(|d| d.remove("pasteboard"))
        .context("missing pasteboard")
}

pub type Revision = (Option<Value>, u64);

pub fn revision(board: &Value) -> Result<Revision> {
    let meta = board
        .as_dictionary()
        .and_then(|d| d.get("metadata"))
        .and_then(Value::as_dictionary)
        .context("missing pasteboard metadata")?;
    Ok((
        meta.get("nonce").cloned(),
        meta.get("changeCount")
            .and_then(Value::as_unsigned_integer)
            .context("missing change counter")?,
    ))
}

pub fn valid(text: &str) -> bool {
    !text.is_empty() && text.len() <= LIMIT && !text.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_copy_requests_advertised_text_without_inline_bytes() {
        let board = plist_macro::plist!({"items": [{
            "types": ["com.apple.WebKit.custom-pasteboard-data", "public.utf8-plain-text", "public.html"],
            "data": plist::Dictionary::new()
        }]});
        let entries = text_entries(&board);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            (entries[0].index, entries[0].uti, entries[0].inline),
            (0, TYPES[0], None)
        );
    }
    #[test]
    fn same_counter_on_a_new_pasteboard_is_a_new_copy() {
        let a = plist_macro::plist!({"metadata":{"nonce":"a","changeCount":4}});
        let b = plist_macro::plist!({"metadata":{"nonce":"b","changeCount":4}});
        assert_ne!(revision(&a).unwrap(), revision(&b).unwrap());
    }
}
