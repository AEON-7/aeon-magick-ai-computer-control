use crate::config::Config;
use crate::input::Hid;
use std::sync::Arc;

pub struct Shared {
    pub cfg: Config,
    pub hid: Hid,
}

#[derive(Clone)]
pub struct SharedState(pub Arc<Shared>);

impl SharedState {
    pub fn new(cfg: Config) -> Self {
        Self(Arc::new(Shared {
            cfg,
            hid: Hid::new(),
        }))
    }
}
