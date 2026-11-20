// Compiled in the isolated player against the project's own BSN protocol.
use bsn_pie_protocol::{ControlEvent, PieChannel, PieTransport};
use ipc_channel::{
    TryRecvError,
    ipc::{self, IpcReceiver, IpcSender},
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum Lane {
    Reliable,
    Frames,
}
#[derive(Serialize, Deserialize)]
struct Frame {
    channel: Lane,
    bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct Bootstrap {
    parent_rx: IpcReceiver<Frame>,
    parent_tx: IpcSender<Frame>,
}

pub fn start() {
    let parent = std::env::var("JACKDAW_PIE").expect("missing editor rendezvous");
    let (server, name) = bsn_pie_protocol::serve().expect("BSN rendezvous");
    // SAFETY: called as the first operation of main, before starting threads.
    unsafe {
        std::env::set_var("BSN_EDITOR_PIE", name);
        std::env::set_var("BSN_EDITOR_PIE_MODE", "play");
        if std::env::var_os("JACKDAW_PIE_WINDOWLESS").is_some() {
            std::env::set_var("BSN_EDITOR_PIE_WINDOWLESS", "1");
        } else {
            std::env::remove_var("BSN_EDITOR_PIE_WINDOWLESS");
        }
    }
    std::thread::spawn(move || {
        let mut game = server.accept().expect("game rendezvous");
        let (to_editor, parent_rx) = ipc::channel().expect("editor output");
        let (parent_tx, from_editor) = ipc::channel::<Frame>().expect("editor input");
        IpcSender::connect(parent)
            .and_then(|tx| {
                tx.send(Bootstrap {
                    parent_rx,
                    parent_tx,
                })
                .map_err(std::io::Error::other)
            })
            .expect("editor rendezvous");
        loop {
            loop {
                match from_editor.try_recv() {
                    Ok(frame) => {
                        if serde_json::from_slice::<ControlEvent>(&frame.bytes).is_ok() {
                            game.send(PieChannel::Reliable, &frame.bytes);
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(_) => return,
                }
            }
            for (lane, bytes) in game.drain_received() {
                let frame = match lane {
                    PieChannel::Frames => {
                        let Some(f) = bsn_pie_protocol::decode_frame(&bytes) else {
                            continue;
                        };
                        Frame {
                            channel: Lane::Frames,
                            bytes: bsn_pie_protocol::encode_frame(
                                f.width, f.height, f.seq, f.pixels,
                            ),
                        }
                    }
                    PieChannel::Reliable if bytes.starts_with(br#"{"CursorState":"#) => Frame {
                        channel: Lane::Reliable,
                        bytes,
                    },
                    _ => continue,
                };
                if to_editor.send(frame).is_err() {
                    return;
                }
            }
            if game.peer_gone() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
}
