//! What a CLAP plugin on Linux asks its host's run loop for: timers (`timer-support`) and file descriptors (`posix-fd-support`). A plugin there has no loop of its own, and one made with DPF or JUCE draws its window on them,
//! or makes none without them. They are served by `window::pump`, which turns while a window is open, so they are the window's: a plugin that asks for a timer with no window open is called when one is.
//!
//! Each plugin's host data (`TangHostMainThread`) keeps what its plugin asked for, and takes it out of the loop when it goes, which is when the plugin is destroyed. A call reaches the plugin through its handle's `access`, which
//! says no once the plugin is being destroyed.

use std::collections::BTreeMap;
use std::os::fd::RawFd;
use std::rc::Rc;

use clack_extensions::posix_fd::{FdFlags, HostPosixFdImpl};
use clack_extensions::timer::{HostTimerImpl, TimerId};
use clack_host::prelude::{HostError, InitializedPluginHandle};
use clap_sys::ext::posix_fd_support::{CLAP_EXT_POSIX_FD_SUPPORT, clap_plugin_posix_fd_support};
use clap_sys::ext::timer_support::{CLAP_EXT_TIMER_SUPPORT, clap_plugin_timer_support};

use crate::window;

/// The timers and file descriptors that one plugin asked for.
#[derive(Default)]
pub struct PluginRunLoop {
    /// The plugin, made `'static`: it is called only through `access`, which says no once it is being destroyed.
    plugin: Option<InitializedPluginHandle<'static>>,
    next_timer: u32,
    timers: BTreeMap<u32, window::Watch>,
    fds: BTreeMap<RawFd, window::Watch>,
}

impl PluginRunLoop {
    pub fn initialized(&mut self, plugin: InitializedPluginHandle<'_>) {
        // SAFETY: only the lifetime is made another; the plugin is reached through `access` alone, which checks that it is not being destroyed, and what was asked of the loop goes with this, before the plugin's memory does.
        self.plugin = Some(unsafe { std::mem::transmute::<InitializedPluginHandle<'_>, InitializedPluginHandle<'static>>(plugin) });
    }

    fn plugin(&self) -> Result<InitializedPluginHandle<'static>, HostError> {
        self.plugin.clone().ok_or(HostError::Message("the plugin is not made yet"))
    }
}

/// Call the plugin's `on_timer`.
fn on_timer(plugin: &InitializedPluginHandle<'static>, id: u32) {
    plugin.access(|shared| {
        let raw = shared.as_raw();
        let Some(get_extension) = raw.get_extension else { return };
        // SAFETY: the plugin's own function, with the plugin, and a name that is a C string; what it gives is the extension's struct or null.
        let extension = unsafe { get_extension(raw, CLAP_EXT_TIMER_SUPPORT.as_ptr()) }.cast::<clap_plugin_timer_support>();
        // SAFETY: as above.
        if let Some(on_timer) = unsafe { extension.as_ref() }.and_then(|extension| extension.on_timer) {
            // SAFETY: the plugin's own function, called on the main thread, which is where the run loop turns.
            unsafe { on_timer(raw, id) };
        }
    });
}

/// Call the plugin's `on_fd`.
fn on_fd(plugin: &InitializedPluginHandle<'static>, fd: RawFd, flags: FdFlags) {
    plugin.access(|shared| {
        let raw = shared.as_raw();
        let Some(get_extension) = raw.get_extension else { return };
        // SAFETY: as in `on_timer`.
        let extension = unsafe { get_extension(raw, CLAP_EXT_POSIX_FD_SUPPORT.as_ptr()) }.cast::<clap_plugin_posix_fd_support>();
        // SAFETY: as above.
        if let Some(on_fd) = unsafe { extension.as_ref() }.and_then(|extension| extension.on_fd) {
            // SAFETY: as above.
            unsafe { on_fd(raw, fd, flags.bits()) };
        }
    });
}

/// The `poll` events for what a plugin wants to hear of a file descriptor (an error is heard whatever is asked).
fn poll_events(flags: FdFlags) -> i16 {
    let mut events = 0;
    if flags.contains(FdFlags::READ) {
        events |= libc::POLLIN;
    }
    if flags.contains(FdFlags::WRITE) {
        events |= libc::POLLOUT;
    }
    events
}

/// What `poll` said of a file descriptor, as the plugin is told it.
fn fd_flags(revents: i16) -> FdFlags {
    let mut flags = FdFlags::empty();
    if revents & libc::POLLIN != 0 {
        flags |= FdFlags::READ;
    }
    if revents & libc::POLLOUT != 0 {
        flags |= FdFlags::WRITE;
    }
    if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        flags |= FdFlags::ERROR;
    }
    flags
}

impl HostTimerImpl for PluginRunLoop {
    fn register_timer(&mut self, period_ms: u32) -> Result<TimerId, HostError> {
        let plugin = self.plugin()?;
        self.next_timer += 1;
        let id = self.next_timer;
        let watch = window::every(std::time::Duration::from_millis(u64::from(period_ms)), Rc::new(move || on_timer(&plugin, id)));
        self.timers.insert(id, watch);
        Ok(TimerId(id))
    }

    fn unregister_timer(&mut self, timer_id: TimerId) -> Result<(), HostError> {
        let watch = self.timers.remove(&timer_id.0).ok_or(HostError::Message("no such timer"))?;
        window::unwatch(watch);
        Ok(())
    }
}

impl HostPosixFdImpl for PluginRunLoop {
    fn register_fd(&mut self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        if self.fds.contains_key(&fd) {
            return Err(HostError::Message("the file descriptor is registered already"));
        }
        let plugin = self.plugin()?;
        let watch = window::watch_fd(fd, poll_events(flags), Rc::new(move |revents| on_fd(&plugin, fd, fd_flags(revents))));
        self.fds.insert(fd, watch);
        Ok(())
    }

    fn modify_fd(&mut self, fd: RawFd, flags: FdFlags) -> Result<(), HostError> {
        self.unregister_fd(fd)?;
        self.register_fd(fd, flags)
    }

    fn unregister_fd(&mut self, fd: RawFd) -> Result<(), HostError> {
        let watch = self.fds.remove(&fd).ok_or(HostError::Message("no such file descriptor"))?;
        window::unwatch(watch);
        Ok(())
    }
}

impl Drop for PluginRunLoop {
    fn drop(&mut self) {
        for watch in self.timers.values().chain(self.fds.values()) {
            window::unwatch(*watch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_plugin_wants_of_a_file_descriptor_is_what_poll_is_asked_and_what_poll_says_is_what_the_plugin_is_told() {
        assert_eq!(poll_events(FdFlags::READ), libc::POLLIN);
        assert_eq!(poll_events(FdFlags::WRITE), libc::POLLOUT);
        assert_eq!(poll_events(FdFlags::READ | FdFlags::WRITE | FdFlags::ERROR), libc::POLLIN | libc::POLLOUT);
        assert_eq!(fd_flags(libc::POLLIN), FdFlags::READ);
        assert_eq!(fd_flags(libc::POLLOUT | libc::POLLHUP), FdFlags::WRITE | FdFlags::ERROR);
        assert_eq!(fd_flags(libc::POLLNVAL), FdFlags::ERROR);
    }
}
