//! The web interface as the TUI's loop sees it (`tang --web`; `docs/web-ui.md` step 13).
//!
//! The loop owns the controller and runs the web runtime's owner among its own
//! business: it waits for the requests the browsers make along with the terminal's
//! events, answers them, reacts to what they changed, and keeps what the connections
//! read current. `Web` is the runtime as far as the loop needs it. Without the `web`
//! feature there is no such thing: `Web` has no values, `--web` is refused before the
//! loop starts, and the loop is what it was.

use std::net::SocketAddr;

/// What the TUI shows of the web interface it serves: in the tab bar and on the Help tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebStatus {
    /// Where it listens.
    pub address: SocketAddr,
    /// The address to open in a browser, with the token in it if there is one.
    pub url: String,
    /// How many browsers are connected.
    pub clients: usize,
}

impl WebStatus {
    /// The tab bar's words for it, the longest of the two that fits in `room`
    /// columns: where it listens and how many browsers are connected, or only that
    /// there is one and how many. None if even that does not fit.
    pub fn label(&self, room: usize) -> Option<String> {
        let clients = if self.clients > 0 { format!(" ({})", self.clients) } else { String::new() };
        [format!("web {}{clients}", self.address), format!("web{clients}")].into_iter().find(|label| label.chars().count() <= room)
    }
}

#[cfg(feature = "web")]
mod enabled {
    use crossbeam_channel::Receiver;

    use super::WebStatus;
    use crate::app::controller::Controller;
    use crate::app::intent::Applied;
    use crate::cli::WebArgs;
    use crate::web::connection::OwnerRequest;
    use crate::web::runtime::Runtime;

    /// What a browser asks of the loop.
    pub type Request = OwnerRequest;

    pub struct Web {
        runtime: Runtime,
    }

    impl Web {
        /// Start serving `ctl`'s session on the address in `args`.
        pub fn start(ctl: &mut Controller, args: &WebArgs) -> anyhow::Result<Web> {
            let runtime = Runtime::start(ctl, crate::web::config_for(args.web_bind, &args.web_origins))?;
            Ok(Web { runtime })
        }

        pub fn status(&self) -> WebStatus {
            WebStatus { address: self.runtime.addr(), url: self.runtime.url(), clients: self.runtime.clients() }
        }

        /// Stop the server as if its thread had died.
        #[cfg(test)]
        pub fn stop_server(&mut self) {
            self.runtime.stop_server();
        }

        /// The requests the browsers make, for the loop to select on.
        pub fn requests(&self) -> Receiver<Request> {
            self.runtime.requests()
        }

        /// Answer `request` and the ones waiting behind it. The intents that were
        /// applied: the view has to react to what a browser changed.
        pub fn serve(&mut self, ctl: &mut Controller, request: Request) -> Vec<Applied> {
            self.runtime.serve(ctl, request)
        }

        /// Keep what the connections read current; call it on every turn of the loop.
        pub fn refresh(&mut self, ctl: &Controller) {
            self.runtime.refresh(ctl);
        }
    }

    /// Tell the browsers about what the TUI has changed without an intent, if it has
    /// (`Controller::resync`): call it when the TUI has handled its input.
    pub fn resync(ctl: &mut Controller) {
        ctl.resync();
    }
}

#[cfg(not(feature = "web"))]
mod disabled {
    use super::WebStatus;
    use crate::app::controller::Controller;
    use crate::app::intent::Applied;
    use crate::cli::WebArgs;

    pub type Request = std::convert::Infallible;

    /// There is no web interface in this build, so there is no value of this type.
    pub enum Web {}

    impl Web {
        /// (`tang --web` is refused before this is reached.)
        pub fn start(_ctl: &mut Controller, _args: &WebArgs) -> anyhow::Result<Web> {
            anyhow::bail!("this build of tang has no web interface: build it with `--features web`")
        }

        pub fn status(&self) -> WebStatus {
            match *self {}
        }

        pub fn requests(&self) -> crossbeam_channel::Receiver<Request> {
            match *self {}
        }

        pub fn serve(&mut self, _ctl: &mut Controller, request: Request) -> Vec<Applied> {
            match request {}
        }

        pub fn refresh(&mut self, _ctl: &Controller) {
            match *self {}
        }
    }

    pub fn resync(_ctl: &mut Controller) {}
}

#[cfg(feature = "web")]
pub use enabled::{Web, resync};
#[cfg(not(feature = "web"))]
pub use disabled::{Web, resync};

#[cfg(test)]
mod tests {
    use super::*;

    fn status(address: &str, clients: usize) -> WebStatus {
        WebStatus { address: address.parse().unwrap(), url: format!("http://{address}/"), clients }
    }

    #[test]
    fn the_label_gives_the_address_and_the_browsers_connected() {
        assert_eq!(status("127.0.0.1:8765", 0).label(100).as_deref(), Some("web 127.0.0.1:8765"));
        assert_eq!(status("127.0.0.1:8765", 2).label(100).as_deref(), Some("web 127.0.0.1:8765 (2)"));
        assert_eq!(status("[::1]:9000", 1).label(100).as_deref(), Some("web [::1]:9000 (1)"));
    }

    #[test]
    fn the_label_is_shortened_to_fit_and_left_out_if_even_that_does_not() {
        let web = status("127.0.0.1:8765", 12);
        assert_eq!(web.label(23).as_deref(), Some("web 127.0.0.1:8765 (12)"));
        assert_eq!(web.label(22).as_deref(), Some("web (12)"), "without the address");
        assert_eq!(web.label(8).as_deref(), Some("web (12)"));
        assert_eq!(web.label(7), None);
        assert_eq!(status("127.0.0.1:8765", 0).label(3).as_deref(), Some("web"));
        assert_eq!(status("127.0.0.1:8765", 0).label(2), None);
    }
}
