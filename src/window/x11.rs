//! The window on Linux: an X11 window by x11rb (a plugin attaches its own window to it as a child, by the window's id: VST3's `X11EmbedWindowID`, CLAP's `x11`), and an event pump that is also the run loop a plugin's
//! window lives on. A VST3 plugin on Linux has no loop of its own: it asks the host to call it when a file descriptor of its own is ready and on a timer (`IRunLoop`), and [`watch_fd`] and [`every`] are those calls.
//!
//! Under Wayland the window is XWayland's, which is where the plugins draw too.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::fd::{AsRawFd, RawFd};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::properties::WmSizeHints;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux, EventMask, ImageFormat, MapState, PropMode, StackMode, Window as XWindow, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{Spec, Window};

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_DELETE_WINDOW,
        UTF8_STRING,
        _NET_WM_NAME,
        _NET_WM_PID,
        _NET_WM_USER_TIME,
        _NET_ACTIVE_WINDOW,
        _NET_CLIENT_LIST,
    }
}

/// The connection to the X server, made the first time a window is asked for, and the windows that are open on it.
struct Display {
    conn: RustConnection,
    screen: usize,
    atoms: Atoms,
    windows: RefCell<BTreeMap<XWindow, Weak<State>>>,
}

thread_local! {
    static DISPLAY: RefCell<Option<Rc<Display>>> = const { RefCell::new(None) };
    static RUN_LOOP: RefCell<RunLoop> = RefCell::new(RunLoop::default());
}

fn x<T>(result: Result<T, impl std::fmt::Display>) -> Result<T, String> {
    result.map_err(|why| format!("X11: {why}"))
}

fn display() -> Result<Rc<Display>, String> {
    DISPLAY.with(|display| {
        if let Some(display) = display.borrow().as_ref() {
            return Ok(display.clone());
        }
        if std::env::var_os("DISPLAY").is_none_or(|name| name.is_empty()) {
            return Err("this session has no screen to open a window on (no DISPLAY: a login over ssh, a container, CI)".to_string());
        }
        let (conn, screen) = x11rb::connect(None).map_err(|why| format!("this session has no screen to open a window on (the X server did not answer: {why})"))?;
        let atoms = x(x(Atoms::new(&conn))?.reply())?;
        let made = Rc::new(Display { conn, screen, atoms, windows: RefCell::new(BTreeMap::new()) });
        *display.borrow_mut() = Some(made.clone());
        Ok(made)
    })
}

/// Whether this machine can show a window, and if it cannot, why: whether an X server answers.
pub fn available() -> Result<(), String> {
    display().map(|_| ())
}

/// Open a window that has `spec`'s size inside it, on the screen.
pub fn open(spec: &Spec) -> Result<Rc<dyn Window>, String> {
    let display = display()?;
    let conn = &display.conn;
    let screen = &conn.setup().roots[display.screen];
    let id = x(conn.generate_id())?;
    let (width, height) = (side(spec.width), side(spec.height));
    let x_pos = (i32::from(screen.width_in_pixels) - i32::from(width)) / 2;
    let y_pos = (i32::from(screen.height_in_pixels) - i32::from(height)) / 2;
    let aux = CreateWindowAux::new().background_pixel(screen.black_pixel).event_mask(EventMask::STRUCTURE_NOTIFY);
    x(conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        id,
        screen.root,
        x_pos.max(0) as i16,
        y_pos.max(0) as i16,
        width,
        height,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &aux,
    ))?;
    let atoms = &display.atoms;
    x(conn.change_property32(PropMode::REPLACE, id, atoms.WM_PROTOCOLS, AtomEnum::ATOM, &[atoms.WM_DELETE_WINDOW]))?;
    x(conn.change_property8(PropMode::REPLACE, id, atoms._NET_WM_NAME, atoms.UTF8_STRING, spec.title.as_bytes()))?;
    x(conn.change_property8(PropMode::REPLACE, id, AtomEnum::WM_NAME, AtomEnum::STRING, spec.title.as_bytes()))?;
    x(conn.change_property8(PropMode::REPLACE, id, AtomEnum::WM_CLASS, AtomEnum::STRING, b"tang\0Tang\0"))?;
    x(conn.change_property32(PropMode::REPLACE, id, atoms._NET_WM_PID, AtomEnum::CARDINAL, &[std::process::id()]))?;
    if !spec.focus {
        x(conn.change_property32(PropMode::REPLACE, id, atoms._NET_WM_USER_TIME, AtomEnum::CARDINAL, &[0]))?;
    }
    let state = Rc::new(State { display: display.clone(), id, size: Cell::new((i32::from(width), i32::from(height))), resizable: spec.resizable, open: Cell::new(true), destroyed: Cell::new(false) });
    state.hint_size()?;
    x(conn.map_window(id))?;
    if spec.focus {
        state.activate()?;
    }
    x(conn.flush())?;
    display.windows.borrow_mut().insert(id, Rc::downgrade(&state));
    Ok(state)
}

/// A side of a window as X11 has it: 1 to what a 16-bit number holds.
fn side(length: i32) -> u16 {
    u16::try_from(length.max(1)).unwrap_or(u16::MAX)
}

/// What a plugin asked to be called for on a timer.
pub type Callback = Rc<dyn Fn()>;

/// What a plugin asked to be called for when a file descriptor of its own is ready: given what it is ready for (`poll`'s `revents`).
pub type FdCallback = Rc<dyn Fn(i16)>;

/// A plugin's place in the run loop, which [`unwatch`] takes away.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Watch(u64);

struct Timer {
    interval: Duration,
    due: Instant,
    call: Callback,
}

#[derive(Default)]
struct RunLoop {
    next: u64,
    fds: BTreeMap<Watch, (RawFd, i16, FdCallback)>,
    timers: BTreeMap<Watch, Timer>,
}

impl RunLoop {
    fn take_id(&mut self) -> Watch {
        self.next += 1;
        Watch(self.next)
    }
}

/// Call `call` from [`pump`] whenever `fd` is ready for one of `events` (`poll`'s: `POLLIN`, `POLLOUT`).
pub fn watch_fd(fd: RawFd, events: i16, call: FdCallback) -> Watch {
    RUN_LOOP.with(|run_loop| {
        let mut run_loop = run_loop.borrow_mut();
        let id = run_loop.take_id();
        run_loop.fds.insert(id, (fd, events, call));
        id
    })
}

/// Call `call` from [`pump`] every `interval` (a millisecond at least).
pub fn every(interval: Duration, call: Callback) -> Watch {
    let interval = interval.max(Duration::from_millis(1));
    RUN_LOOP.with(|run_loop| {
        let mut run_loop = run_loop.borrow_mut();
        let id = run_loop.take_id();
        run_loop.timers.insert(id, Timer { interval, due: Instant::now() + interval, call });
        id
    })
}

/// Stop calling what `watch` calls. Nothing to do once the thread's run loop is gone: a plugin that is let go of while the program ends asks this after it.
pub fn unwatch(watch: Watch) {
    let _ = RUN_LOOP.try_with(|run_loop| {
        let mut run_loop = run_loop.borrow_mut();
        run_loop.fds.remove(&watch);
        run_loop.timers.remove(&watch);
    });
}

/// Let the windows have their events and the plugins their file descriptors and timers, for `limit` if nothing comes and until there is nothing more to do if something does.
pub fn pump(limit: Duration) {
    let display = DISPLAY.with(|display| display.borrow().clone());
    let deadline = Instant::now() + limit;
    loop {
        let mut handled = display.as_ref().is_some_and(|display| display.handle_events());
        let (fds, next_timer) = RUN_LOOP.with(|run_loop| {
            let run_loop = run_loop.borrow();
            let fds: Vec<(Watch, RawFd, i16)> = run_loop.fds.iter().map(|(&id, &(fd, events, _))| (id, fd, events)).collect();
            (fds, run_loop.timers.values().map(|timer| timer.due).min())
        });
        let mut wait = next_timer.map_or(deadline, |due| due.min(deadline)).saturating_duration_since(Instant::now());
        if handled {
            wait = Duration::ZERO;
        }
        let mut polled: Vec<libc::pollfd> = fds.iter().map(|&(_, fd, events)| libc::pollfd { fd, events, revents: 0 }).collect();
        if let Some(display) = &display {
            polled.push(libc::pollfd { fd: display.conn.stream().as_raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let millis = libc::c_int::try_from(wait.as_micros().div_ceil(1000)).unwrap_or(libc::c_int::MAX);
        // Safety: the array is ours, and its length is what is said.
        let ready = unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, millis) };
        if ready > 0 {
            for (&(watch, _, _), pollfd) in fds.iter().zip(&polled) {
                if pollfd.revents != 0 {
                    // (Asked again: an earlier call may have taken it out of the loop.)
                    let call = RUN_LOOP.with(|run_loop| run_loop.borrow().fds.get(&watch).map(|(_, _, call)| call.clone()));
                    if let Some(call) = call {
                        call(pollfd.revents);
                        handled = true;
                    }
                }
            }
        }
        handled |= fire_timers();
        if let Some(display) = &display {
            handled |= display.handle_events();
        }
        if !handled || Instant::now() >= deadline {
            break;
        }
    }
}

/// Call the timers that are due, each once however late it is. Whether any was.
fn fire_timers() -> bool {
    let now = Instant::now();
    let due: Vec<(Watch, Callback)> = RUN_LOOP.with(|run_loop| {
        let mut run_loop = run_loop.borrow_mut();
        run_loop
            .timers
            .iter_mut()
            .filter(|(_, timer)| timer.due <= now)
            .map(|(&id, timer)| {
                timer.due = (timer.due + timer.interval).max(now);
                (id, timer.call.clone())
            })
            .collect()
    });
    let mut fired = false;
    for (watch, call) in due {
        let still = RUN_LOOP.with(|run_loop| run_loop.borrow().timers.contains_key(&watch));
        if still {
            call();
            fired = true;
        }
    }
    fired
}

impl Display {
    /// Hand the events that have come to the windows they are for. Whether there were any.
    fn handle_events(&self) -> bool {
        let _ = self.conn.flush();
        let mut any = false;
        while let Ok(Some(event)) = self.conn.poll_for_event() {
            any = true;
            let (id, what) = match event {
                Event::ConfigureNotify(event) => (event.window, Happened::Resized(i32::from(event.width), i32::from(event.height))),
                Event::ClientMessage(event) if event.type_ == self.atoms.WM_PROTOCOLS && event.format == 32 && event.data.as_data32()[0] == self.atoms.WM_DELETE_WINDOW => (event.window, Happened::CloseAsked),
                Event::DestroyNotify(event) => (event.window, Happened::Destroyed),
                _ => continue,
            };
            let state = self.windows.borrow().get(&id).and_then(Weak::upgrade);
            let Some(state) = state else { continue };
            match what {
                Happened::Resized(width, height) => state.size.set((width, height)),
                Happened::CloseAsked => state.hide(),
                Happened::Destroyed => {
                    state.destroyed.set(true);
                    state.close();
                }
            }
        }
        any
    }
}

/// What the X server said of one of the windows.
enum Happened {
    Resized(i32, i32),
    /// The person closed it: it is hidden, and destroyed once its owner has taken the plugin's window out of it (`close`). Destroyed at once, it would take the plugin's window with it while the plugin still draws in it, and a
    /// plugin that uses Xlib is then told of a window that is not there, which Xlib answers by ending the program.
    CloseAsked,
    Destroyed,
}

/// A window on the X server: the one that is open, and what a plugin's window is a child of.
struct State {
    display: Rc<Display>,
    id: XWindow,
    /// The size inside the window, in pixels: what it was made, set to, or what the window manager says it is now.
    size: Cell<(i32, i32)>,
    resizable: bool,
    open: Cell<bool>,
    destroyed: Cell<bool>,
}

impl State {
    /// Take the window off the screen and say that it is closed, but leave it there for the plugin's window that is in it until its owner closes it.
    fn hide(&self) {
        self.open.set(false);
        let _ = self.display.conn.unmap_window(self.id);
        let _ = self.display.conn.flush();
    }

    /// Tell the window manager what sizes the window may be: any, or the one that it is.
    fn hint_size(&self) -> Result<(), String> {
        let (width, height) = self.size.get();
        let mut hints = WmSizeHints::new();
        if !self.resizable {
            hints.min_size = Some((width, height));
            hints.max_size = Some((width, height));
        }
        x(hints.set_normal_hints(&self.display.conn, self.id))?;
        Ok(())
    }

    /// Ask the window manager to make the window the active one, which gives it the keyboard.
    fn activate(&self) -> Result<(), String> {
        let display = &self.display;
        let root = display.conn.setup().roots[display.screen].root;
        let message = ClientMessageEvent::new(32, self.id, display.atoms._NET_ACTIVE_WINDOW, [1u32, x11rb::CURRENT_TIME, 0, 0, 0]);
        x(display.conn.send_event(false, root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, message))?;
        Ok(())
    }

    fn describe_tree(&self, window: XWindow, depth: usize, text: &mut String) {
        let conn = &self.display.conn;
        let indent = "  ".repeat(depth);
        let geometry = conn.get_geometry(window).ok().and_then(|cookie| cookie.reply().ok());
        let mapped = conn.get_window_attributes(window).ok().and_then(|cookie| cookie.reply().ok()).map(|attributes| attributes.map_state);
        let (width, height) = geometry.map_or((0, 0), |geometry| (geometry.width, geometry.height));
        let hidden = match mapped {
            Some(MapState::VIEWABLE) => "",
            _ => " (not mapped)",
        };
        text.push_str(&format!("{indent}window 0x{window:x} {width}x{height}{hidden}\n"));
        if depth == DESCRIBE_DEPTH {
            text.push_str(&format!("{indent}  ...\n"));
            return;
        }
        let children = conn.query_tree(window).ok().and_then(|cookie| cookie.reply().ok()).map(|tree| tree.children).unwrap_or_default();
        for child in children {
            self.describe_tree(child, depth + 1, text);
        }
    }

    /// The other top-level windows of this program that the window manager knows of: a plugin that makes a window of its own instead of putting it in ours shows here.
    fn other_windows(&self) -> Vec<String> {
        let display = &self.display;
        let conn = &display.conn;
        let root = conn.setup().roots[display.screen].root;
        let property = |window: XWindow, name, kind| conn.get_property(false, window, name, kind, 0, 1024).ok().and_then(|cookie| cookie.reply().ok());
        let Some(clients) = property(root, display.atoms._NET_CLIENT_LIST, AtomEnum::WINDOW.into()) else {
            return Vec::new();
        };
        let pid = std::process::id();
        clients
            .value32()
            .into_iter()
            .flatten()
            .filter(|&client| client != self.id)
            .filter(|&client| property(client, display.atoms._NET_WM_PID, AtomEnum::CARDINAL.into()).and_then(|reply| reply.value32().and_then(|mut values| values.next())) == Some(pid))
            .map(|client| {
                let name = property(client, display.atoms._NET_WM_NAME, display.atoms.UTF8_STRING).map(|reply| String::from_utf8_lossy(&reply.value).into_owned()).unwrap_or_default();
                format!("window 0x{client:x} \"{name}\"")
            })
            .collect()
    }
}

/// How deep a window tree is described: a plugin's windows are a few levels, and this is for a person to read.
const DESCRIBE_DEPTH: usize = 12;

impl Window for State {
    fn parent(&self) -> *mut c_void {
        self.id as usize as *mut c_void
    }

    fn size(&self) -> (i32, i32) {
        self.size.get()
    }

    fn set_size(&self, width: i32, height: i32) {
        if !self.open.get() {
            return;
        }
        let (width, height) = (side(width), side(height));
        self.size.set((i32::from(width), i32::from(height)));
        let conn = &self.display.conn;
        let _ = self.hint_size();
        let _ = conn.configure_window(self.id, &ConfigureWindowAux::new().width(u32::from(width)).height(u32::from(height)));
        let _ = conn.flush();
    }

    fn is_open(&self) -> bool {
        self.open.get()
    }

    fn raise(&self, focus: bool) {
        if !self.open.get() {
            return;
        }
        let conn = &self.display.conn;
        let _ = conn.map_window(self.id);
        let _ = conn.configure_window(self.id, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        if focus {
            let _ = self.activate();
        }
        let _ = conn.flush();
    }

    fn close(&self) {
        self.open.set(false);
        self.display.windows.borrow_mut().remove(&self.id);
        if self.destroyed.replace(true) {
            return;
        }
        let _ = self.display.conn.destroy_window(self.id);
        let _ = self.display.conn.flush();
    }

    fn snapshot_png(&self) -> Option<Vec<u8>> {
        let (width, height) = self.size.get();
        let (width, height) = (side(width), side(height));
        let conn = &self.display.conn;
        let image = conn.get_image(ImageFormat::Z_PIXMAP, self.id, 0, 0, width, height, !0).ok()?.reply().ok()?;
        let format = conn.setup().pixmap_formats.iter().find(|format| format.depth == image.depth)?;
        if format.bits_per_pixel != 32 || conn.setup().image_byte_order != x11rb::protocol::xproto::ImageOrder::LSB_FIRST {
            return None;
        }
        let pixels = usize::from(width) * usize::from(height);
        let mut rgb = Vec::with_capacity(pixels * 3);
        let stride = (usize::from(width) * 32).div_ceil(usize::from(format.scanline_pad)) * usize::from(format.scanline_pad) / 8;
        for row in image.data.chunks(stride).take(usize::from(height)) {
            for pixel in row.as_chunks::<4>().0.iter().take(usize::from(width)) {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
        }
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, u32::from(width), u32::from(height));
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().ok()?.write_image_data(&rgb).ok()?;
        Some(png)
    }

    fn describe(&self) -> String {
        let mut text = String::new();
        self.describe_tree(self.id, 0, &mut text);
        let others = self.other_windows();
        if others.is_empty() {
            text.push_str("no other windows\n");
        } else {
            text.push_str(&format!("other windows: {}\n", others.join(", ")));
        }
        text
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // A window that nobody holds is not left on the screen.
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    #[ignore = "opens a window on the screen: for a person at a Linux desktop"]
    fn a_window_that_the_person_closes_keeps_the_plugins_window_in_it_until_its_owner_closes_it() {
        let window = open(&Spec { title: "a test", width: 200, height: 100, resizable: false, focus: false }).expect("a screen");
        let (plugin, _) = x11rb::connect(None).expect("a second connection, as a plugin has");
        let child = plugin.generate_id().expect("an id");
        plugin.create_window(x11rb::COPY_DEPTH_FROM_PARENT, child, window.parent() as usize as XWindow, 0, 0, 200, 100, 0, WindowClass::INPUT_OUTPUT, x11rb::COPY_FROM_PARENT, &CreateWindowAux::new()).expect("the request");
        plugin.flush().expect("sent");
        let atoms = display().expect("the display").atoms;
        let delete = ClientMessageEvent::new(32, window.parent() as usize as XWindow, atoms.WM_PROTOCOLS, [atoms.WM_DELETE_WINDOW, 0, 0, 0, 0]);
        plugin.send_event(false, window.parent() as usize as XWindow, EventMask::NO_EVENT, delete).expect("the request");
        plugin.flush().expect("sent");
        pump(Duration::from_millis(200));
        assert!(!window.is_open(), "the person closed it");
        assert!(plugin.get_geometry(child).expect("the request").reply().is_ok(), "the plugin's window is still there for it to be taken out of");
        window.close();
        pump(Duration::from_millis(50));
        assert!(plugin.get_geometry(child).expect("the request").reply().is_err(), "and goes with the window once its owner closes it");
    }

    #[test]
    fn a_timer_is_called_by_the_pump_when_it_is_due_and_not_after_it_is_taken_out() {
        let calls = Rc::new(Cell::new(0));
        let counted = calls.clone();
        let watch = every(Duration::from_millis(5), Rc::new(move || counted.set(counted.get() + 1)));
        pump(Duration::from_millis(2));
        assert_eq!(calls.get(), 0, "not due yet");
        pump(Duration::from_millis(30));
        assert!(calls.get() >= 1, "due within the pump's time");
        unwatch(watch);
        let before = calls.get();
        pump(Duration::from_millis(20));
        assert_eq!(calls.get(), before);
    }

    #[test]
    fn a_timer_that_takes_itself_out_while_it_is_called_is_not_called_again() {
        let calls = Rc::new(Cell::new(0));
        let mine: Rc<Cell<Option<Watch>>> = Rc::new(Cell::new(None));
        let (counted, its_own) = (calls.clone(), mine.clone());
        let watch = every(
            Duration::from_millis(1),
            Rc::new(move || {
                counted.set(counted.get() + 1);
                if let Some(watch) = its_own.get() {
                    unwatch(watch);
                }
            }),
        );
        mine.set(Some(watch));
        pump(Duration::from_millis(20));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_file_descriptor_that_is_ready_is_handed_to_whoever_watches_it_with_what_it_is_ready_for() {
        let mut ends = [0; 2];
        // Safety: the array is ours.
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let heard = Rc::new(Cell::new(0i16));
        let told = heard.clone();
        let watch = watch_fd(ends[0], libc::POLLIN, Rc::new(move |revents| told.set(revents)));
        pump(Duration::from_millis(5));
        assert_eq!(heard.get(), 0, "nothing written yet");
        // Safety: a byte of ours, to the pipe's write end.
        assert_eq!(unsafe { libc::write(ends[1], b"x".as_ptr().cast(), 1) }, 1);
        pump(Duration::from_millis(50));
        assert_ne!(heard.get() & libc::POLLIN, 0);
        unwatch(watch);
        // Safety: the pipe is ours.
        unsafe {
            libc::close(ends[0]);
            libc::close(ends[1]);
        }
    }

    #[test]
    fn a_pump_with_nothing_to_do_waits_its_time_and_no_longer() {
        let started = Instant::now();
        pump(Duration::from_millis(20));
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(20) && took < Duration::from_millis(500), "{took:?}");
    }
}
