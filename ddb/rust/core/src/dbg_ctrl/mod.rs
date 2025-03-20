pub mod dbg_ctrl;
pub mod dbg_bridge_ctrl;
use bytes::Bytes;
pub use dbg_ctrl::*;

// pub type ChannelSender = tokio::sync::mpsc::Sender<Bytes>;
// pub type ChannelReceiver = tokio::sync::mpsc::Receiver<Bytes>;
pub type ChannelSender = flume::Sender<Bytes>;
pub type ChannelReceiver = flume::Receiver<Bytes>;

pub type InputSender = ChannelSender;
pub type InputReceiver = ChannelReceiver;

// used to send output to a centralize location for processing
pub type OutputSender = ChannelSender;
// used to receive output at a centralize location for processing
pub type OutputReceiver = ChannelReceiver;
