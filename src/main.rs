use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::LazyLock;
use std::{fs, ptr, thread};
use std::time::Duration;
use std::path::PathBuf;

use configuration::{Configuration, APP_CONFIG};
use gdk_pixbuf::gio::ApplicationFlags;
use gdk_pixbuf::{Pixbuf, PixbufLoader, Colorspace};
use gdk_pixbuf::glib::Bytes;
use gtk::prelude::*;

use relm4::gtk::gdk::Rectangle;

use relm4::{
    gtk::{self, gdk::DisplayManager, CssProvider, Window},
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmApp,
};

use anyhow::{anyhow, Context, Result};

use sketch_board::{SketchBoardOutput, SketchBoardInput};
use ui::toolbars::{StyleToolbar, StyleToolbarInput, ToolsToolbar, ToolsToolbarInput};
use xdg::BaseDirectories;

mod configuration;
mod femtovg_area;
mod icons;
mod ime;
mod math;
mod notification;
mod sketch_board;
mod style;
mod tools;
mod ui;

use crate::sketch_board::SketchBoard;
use crate::tools::Tools;

pub static START_TIME: LazyLock<chrono::DateTime<chrono::Local>> =
    LazyLock::new(chrono::Local::now);

#[derive(Debug, Clone)]
struct RawImageData {
    width: i32,
    height: i32,
    n_channels: i32,
    rowstride: i32,
    data: Vec<u8>,
}

fn get_socket_path() -> PathBuf {
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("satty-{}.sock", uid))
}

fn try_send_to_daemon(image: &Pixbuf) -> bool {
    let socket_path = get_socket_path();
    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let width = image.width();
    let height = image.height();
    let n_channels = image.n_channels();
    let rowstride = image.rowstride();
    
    let byte_struct = image.read_pixel_bytes();
    let pixels = byte_struct.as_ref();

    if stream.write_all(&width.to_be_bytes()).is_err() { return false; }
    if stream.write_all(&height.to_be_bytes()).is_err() { return false; }
    if stream.write_all(&n_channels.to_be_bytes()).is_err() { return false; }
    if stream.write_all(&rowstride.to_be_bytes()).is_err() { return false; }
    if stream.write_all(&(pixels.len() as u64).to_be_bytes()).is_err() { return false; }
    if stream.write_all(pixels).is_err() { return false; }

    true
}

fn read_raw_image_from_stream(mut stream: UnixStream) -> Option<RawImageData> {
    let mut u32_buf = [0u8; 4];
    let mut u64_buf = [0u8; 8];

    stream.read_exact(&mut u32_buf).ok()?;
    let width = i32::from_be_bytes(u32_buf);

    stream.read_exact(&mut u32_buf).ok()?;
    let height = i32::from_be_bytes(u32_buf);

    stream.read_exact(&mut u32_buf).ok()?;
    let n_channels = i32::from_be_bytes(u32_buf);

    stream.read_exact(&mut u32_buf).ok()?;
    let rowstride = i32::from_be_bytes(u32_buf);

    stream.read_exact(&mut u64_buf).ok()?;
    let data_len = u64::from_be_bytes(u64_buf) as usize;

    let mut buffer = vec![0u8; data_len];
    stream.read_exact(&mut buffer).ok()?;

    Some(RawImageData {
        width,
        height,
        n_channels,
        rowstride,
        data: buffer,
    })
}

macro_rules! generate_profile_output {
    ($e: expr) => {
        if (APP_CONFIG.read().profile_startup()) {
            eprintln!(
                "{:5} ms time elapsed: {}",
                (chrono::Local::now() - *START_TIME).num_milliseconds(),
                $e
            );
        }
    };
}

struct App {
    image_dimensions: (i32, i32),
    sketch_board: Controller<SketchBoard>,
    tools_toolbar: Controller<ToolsToolbar>,
    style_toolbar: Controller<StyleToolbar>,
    is_daemon: bool,
}

#[derive(Debug)]
enum AppInput {
    Realized,
    SetToolbarsDisplay(bool),
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
    ColorSwitchShortcut(u64),
    LoadImage(RawImageData),
    Exit,
}

#[derive(Debug)]
enum AppCommandOutput {
    ResetResizable,
}

impl App {
    fn get_monitor_size(root: &Window) -> Option<Rectangle> {
        root.surface().and_then(|surface| {
            DisplayManager::get()
                .default_display()
                .and_then(|display| display.monitor_at_surface(&surface))
                .map(|monitor| monitor.geometry())
        })
    }

    fn resize_window_initial(&self, root: &Window, sender: ComponentSender<Self>) {
        let monitor_size = match Self::get_monitor_size(root) {
            Some(s) => s,
            None => {
                root.set_default_size(self.image_dimensions.0, self.image_dimensions.1);
                return;
            }
        };

        let reduced_monitor_width = monitor_size.width() as f64 * 0.8;
        let reduced_monitor_height = monitor_size.height() as f64 * 0.8;

        let image_width = self.image_dimensions.0 as f64;
        let image_height = self.image_dimensions.1 as f64;

        if reduced_monitor_width > image_width && reduced_monitor_height > image_height {
            root.set_default_size(self.image_dimensions.0, self.image_dimensions.1);
        } else {
            let aspect_ratio = image_width / image_height;
            let mut new_width = reduced_monitor_width;
            let mut new_height = new_width / aspect_ratio;

            if new_height > reduced_monitor_height {
                new_height = reduced_monitor_height;
                new_width = new_height * aspect_ratio;
            }

            root.set_default_size(new_width as i32, new_height as i32);
        }

        root.set_resizable(false);

        if APP_CONFIG.read().fullscreen() {
            root.fullscreen();
        }

        sender.command(|out, shutdown| {
            shutdown
                .register(async move {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    out.emit(AppCommandOutput::ResetResizable);
                })
                .drop_on_shutdown()
        });
    }

    fn apply_style() {
        let css_provider = CssProvider::new();
        css_provider.load_from_data(
            "
            .root {
                min-width: 50rem;
                min-height: 10rem;
            }
            .toolbar {color: #f9f9f9 ; background: #00000099;}
            .toast {
                color: #f9f9f9;
                background: #00000099;
                border-radius: 6px;
                margin-top: 50px;
            }
            .toolbar-bottom {border-radius: 6px 6px 0px 0px;}
            .toolbar-top {border-radius: 0px 0px 6px 6px;}
            ",
        );
        if let Some(overrides) = read_css_overrides() {
            css_provider.load_from_data(&overrides);
        }
        match DisplayManager::get().default_display() {
            Some(display) => {
                gtk::style_context_add_provider_for_display(&display, &css_provider, 1)
            }
            None => println!("Cannot apply style"),
        }
    }
}

#[relm4::component]
impl Component for App {
    type Init = Option<Pixbuf>; 
    type Input = AppInput;
    type Output = ();
    type CommandOutput = AppCommandOutput;

    view! {
        main_window = gtk::Window {
            set_decorated: !APP_CONFIG.read().no_window_decoration(),
            set_default_size: (500, 500),
            add_css_class: "root",
            
            // ИСПРАВЛЕНИЕ 1: используем set_visible вместо visible
            set_visible: false,

            // ИСПРАВЛЕНИЕ 2: захватываем [sender], так как он существует в области видимости.
            // Внутри замыкания он не используется, но это стандартный способ захвата в макросах Relm4.
            connect_close_request[sender] => move |window| {
                if model.is_daemon {
                    window.set_visible(false);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            },

            connect_show[sender] => move |_| {
                generate_profile_output!("gui show event");
                sender.input(AppInput::Realized);
            },
            
            gtk::Overlay {
                add_overlay = model.tools_toolbar.widget(),
                add_overlay = model.style_toolbar.widget(),
                model.sketch_board.widget(),
            }
        }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AppInput::Exit => {
                // Закрываем окно. Поведение определится в connect_close_request
                root.close();
            }
            AppInput::LoadImage(raw_img) => {
                self.image_dimensions = (raw_img.width, raw_img.height);
                
                let bytes = Bytes::from(&raw_img.data);
                let pixbuf = Pixbuf::from_bytes(
                    &bytes,
                    Colorspace::Rgb,
                    raw_img.n_channels == 4,
                    8,
                    raw_img.width,
                    raw_img.height,
                    raw_img.rowstride
                );

                self.sketch_board.sender().emit(SketchBoardInput::LoadImage(pixbuf));
                
                root.set_visible(true); 
                root.present();
                self.resize_window_initial(root, sender);
            }
            AppInput::Realized => self.resize_window_initial(root, sender),
            AppInput::SetToolbarsDisplay(visible) => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::SetVisibility(visible));
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::SetVisibility(visible));
            }
            AppInput::ToggleToolbarsDisplay => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::ToggleVisibility);
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::ToggleVisibility);
            }
            AppInput::ToolSwitchShortcut(tool) => {
                self.tools_toolbar
                    .sender()
                    .emit(ToolsToolbarInput::SwitchSelectedTool(tool));
            }
            AppInput::ColorSwitchShortcut(index) => {
                self.style_toolbar
                    .sender()
                    .emit(StyleToolbarInput::ColorButtonSelected(
                        ui::toolbars::ColorButtons::Palette(index),
                    ));
            }
        }
    }

    fn update_cmd(
        &mut self,
        command: AppCommandOutput,
        _: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match command {
            AppCommandOutput::ResetResizable => root.set_resizable(true),
        }
    }

    fn init(
        image_opt: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        Self::apply_style();

        let is_daemon = image_opt.is_none();

        if is_daemon {
            let sender = sender.clone();
            thread::spawn(move || {
                let socket_path = get_socket_path();
                if socket_path.exists() {
                    let _ = fs::remove_file(&socket_path);
                }
                
                if let Ok(listener) = UnixListener::bind(&socket_path) {
                    for stream in listener.incoming() {
                        if let Ok(stream) = stream {
                            if let Some(raw_img) = read_raw_image_from_stream(stream) {
                                sender.input(AppInput::LoadImage(raw_img));
                            }
                        }
                    }
                } else {
                    eprintln!("Failed to bind socket: {:?}", socket_path);
                }
            });
        }

        let image_dimensions = if let Some(ref img) = image_opt {
             (img.width(), img.height())
        } else {
             (500, 500)
        };

        // SketchBoard
        let sketch_board =
            SketchBoard::builder()
                .launch(image_opt.clone())
                .forward(sender.input_sender(), |t| match t {
                    SketchBoardOutput::ToggleToolbarsDisplay => AppInput::ToggleToolbarsDisplay,
                    SketchBoardOutput::ToolSwitchShortcut(tool) => {
                        AppInput::ToolSwitchShortcut(tool)
                    }
                    SketchBoardOutput::ColorSwitchShortcut(index) => {
                        AppInput::ColorSwitchShortcut(index)
                    }
                    SketchBoardOutput::Exit => AppInput::Exit,
                });

        // Toolbars
        let tools_toolbar = ToolsToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        let style_toolbar = StyleToolbar::builder()
            .launch(())
            .forward(sketch_board.sender(), SketchBoardInput::ToolbarEvent);

        // Model
        let model = App {
            sketch_board,
            tools_toolbar,
            style_toolbar,
            image_dimensions,
            is_daemon,
        };

        let widgets = view_output!();

        if APP_CONFIG.read().focus_toggles_toolbars() {
            let motion_controller = gtk::EventControllerMotion::builder().build();
            let sender_clone = sender.clone();

            motion_controller.connect_enter(move |_, _, _| {
                sender.input(AppInput::SetToolbarsDisplay(true));
            });
            motion_controller.connect_leave(move |_| {
                sender_clone.input(AppInput::SetToolbarsDisplay(false));
            });

            root.add_controller(motion_controller);
        }

        generate_profile_output!("app init end");

        let root_clone = root.clone();
        glib::idle_add_local_once(move || {
            generate_profile_output!("main loop idle");
            
            // ХАК: Relm4 любит показывать окно сам после init.
            // Мы принудительно скрываем его на первом такте цикла, если мы демон.
            if is_daemon {
                root_clone.set_visible(false);
            } else {
                // Если не демон - показываем
                root_clone.set_visible(true);
            }
        });

        ComponentParts { model, widgets }
    }
}

fn read_css_overrides() -> Option<String> {
    let dirs = BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
    let path = dirs.get_config_file("overrides.css")?;

    if !path.exists() {
        return None;
    }

    match fs::read_to_string(&path) {
        Ok(content) => Some(content),
        Err(e) => {
            eprintln!("failed to read CSS overrides: {}", e);
            None
        }
    }
}

fn load_gl() -> Result<()> {
    #[cfg(target_os = "macos")]
    let library = unsafe { libloading::os::unix::Library::new("libepoxy.0.dylib") }?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let library = unsafe { libloading::os::unix::Library::new("libepoxy.so.0") }?;
    #[cfg(windows)]
    let library = libloading::os::windows::Library::open_already_loaded("libepoxy-0.dll")
        .or_else(|_| libloading::os::windows::Library::open_already_loaded("epoxy-0.dll"))?;

    epoxy::load_with(|name| {
        unsafe { library.get::<_>(name.as_bytes()) }
            .map(|symbol| *symbol)
            .unwrap_or(ptr::null())
    });

    Ok(())
}

fn run_satty() -> Result<()> {
    load_gl()?;
    generate_profile_output!("loaded gl");

    let config = APP_CONFIG.read();

    if config.daemon_mode() {
        let socket_path = get_socket_path();
        
        if UnixStream::connect(&socket_path).is_ok() {
            eprintln!("Satty daemon is already running!");
            return Ok(());
        }

        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        generate_profile_output!("starting in DAEMON mode");
        
        let app = relm4::main_application();
        app.set_application_id(Some("com.gabm.satty"));
        app.set_flags(ApplicationFlags::NON_UNIQUE);
        
        let app = RelmApp::from_app(app).with_args(vec![]);
        relm4_icons::initialize_icons(
            icons::icon_names::GRESOURCE_BYTES,
            icons::icon_names::RESOURCE_PREFIX,
        );
        
        app.run::<App>(None);

        if socket_path.exists() {
            let _ = fs::remove_file(socket_path);
        }
        return Ok(());
    }

    generate_profile_output!("loading image");
    
    let image_result = if config.input_filename() == "-" {
        let mut buf = Vec::<u8>::new();
        match io::stdin().lock().read_to_end(&mut buf) {
            Ok(_) if !buf.is_empty() => {
                 let pb_loader = PixbufLoader::new();
                 pb_loader.write(&buf)?;
                 pb_loader.close()?;
                 pb_loader.pixbuf().context("Conversion to Pixbuf failed")
            }
            _ => Err(anyhow!("No input data provided. Use --daemon or provide a file/stdin.")),
        }
    } else {
        Pixbuf::from_file(config.input_filename()).context("couldn't load image")
    };

    match image_result {
        Ok(image) => {
            if try_send_to_daemon(&image) {
                generate_profile_output!("Sent to daemon, exiting");
                return Ok(());
            }

            generate_profile_output!("starting gui (standalone)");
            
            let app = relm4::main_application();
            app.set_application_id(Some("com.gabm.satty"));
            app.set_flags(ApplicationFlags::NON_UNIQUE);
            
            let app = RelmApp::from_app(app).with_args(vec![]);
            relm4_icons::initialize_icons(
                icons::icon_names::GRESOURCE_BYTES,
                icons::icon_names::RESOURCE_PREFIX,
            );
            
            app.run::<App>(Some(image));
            
            Ok(())
        },
        Err(e) => {
            eprintln!("Error: {}", e);
            Err(e)
        }
    }
}

fn main() -> Result<()> {
    let _ = *START_TIME;
    Configuration::load();
    if APP_CONFIG.read().profile_startup() {
        eprintln!(
            "startup timestamp was {}",
            START_TIME.format("%s.%f %Y-%m-%d %H:%M:%S")
        );
    }
    generate_profile_output!("configuration loaded");

    match run_satty() {
        Err(_e) => {
            std::process::exit(1);
        }
        Ok(v) => Ok(v),
    }
}