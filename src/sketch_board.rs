use anyhow::anyhow;

use femtovg::imgref::Img;
use femtovg::rgb::{ComponentBytes, RGBA};
use gdk_pixbuf::glib::Bytes;
use gdk_pixbuf::{Pixbuf, Colorspace};
use keycode::{KeyMap, KeyMappingId};
use std::cell::RefCell;
use std::io::Write;
use std::panic;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::{fs, io, thread};

use gtk::prelude::*;

use relm4::gtk::gdk::{DisplayManager, Key, ModifierType, Texture};
use relm4::{gtk, Component, ComponentParts, ComponentSender, RelmWidgetExt};

use crate::configuration::{Action, APP_CONFIG};
use crate::femtovg_area::FemtoVGArea;
use crate::ime::pango_adapter::spans_from_pango_attrs;
use crate::math::Vec2D;
use crate::notification::log_result;
use crate::style::Style;
use crate::tools::{Tool, ToolEvent, ToolUpdateResult, Tools, ToolsManager};
use crate::ui::toolbars::ToolbarEvent;

use image::{ImageBuffer, Rgba};

type RenderedImage = Img<Vec<RGBA<u8>>>;

#[derive(Debug, Clone)]
pub enum SketchBoardInput {
    InputEvent(InputEvent),
    ToolbarEvent(ToolbarEvent),
    RenderResult(RenderedImage, Vec<Action>),
    CommitEvent(TextEventMsg),
    Refresh,
    LoadImage(Pixbuf),
}

#[derive(Debug, Clone)]
pub enum SketchBoardOutput {
    ToggleToolbarsDisplay,
    ToolSwitchShortcut(Tools),
    ColorSwitchShortcut(u64),
    Exit,
}

#[derive(Debug, Clone)]
pub enum InputEvent {
    Mouse(MouseEventMsg),
    Key(KeyEventMsg),
    KeyRelease(KeyEventMsg),
    Text(TextEventMsg),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum MouseButton {
    Primary,
    Secondary,
    Middle,
}

#[derive(Debug, Clone, Copy)]
pub struct KeyEventMsg {
    pub key: Key,
    pub code: u32,
    pub modifier: ModifierType,
}
#[derive(Debug, Clone)]
pub enum TextEventMsg {
    Commit(String),
    Preedit {
        text: String,
        cursor_chars: Option<usize>,
        spans: Vec<crate::ime::preedit::PreeditSpan>,
    },
    PreeditEnd,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseEventType {
    BeginDrag,
    EndDrag,
    UpdateDrag,
    Click,
    Scroll,
    PointerPos,
    Release,
}

#[derive(Debug, Clone, Copy)]
pub struct MouseEventMsg {
    pub type_: MouseEventType,
    pub button: MouseButton,
    pub modifier: ModifierType,
    pub pos: Vec2D,
    pub n_pressed: i32,
    pub release: bool,
}

impl SketchBoardInput {
    pub fn new_mouse_event(
        event_type: MouseEventType,
        button: u32,
        n_pressed: i32,
        modifier: ModifierType,
        pos: Vec2D,
        release: bool,
    ) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: event_type,
            button: button.into(),
            n_pressed,
            modifier,
            pos,
            release,
        }))
    }
    pub fn new_key_event(event: KeyEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Key(event))
    }

    pub fn new_key_release_event(event: KeyEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::KeyRelease(event))
    }

    pub fn new_text_event(event: TextEventMsg) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Text(event))
    }

    pub fn new_commit_event(event: TextEventMsg) -> SketchBoardInput {
        SketchBoardInput::CommitEvent(event)
    }

    pub fn new_scroll_event(delta_y: f64) -> SketchBoardInput {
        SketchBoardInput::InputEvent(InputEvent::Mouse(MouseEventMsg {
            type_: MouseEventType::Scroll,
            button: MouseButton::Middle,
            n_pressed: 0,
            modifier: ModifierType::empty(),
            pos: Vec2D::new(0.0, delta_y as f32),
            release: false,
        }))
    }
}

impl From<u32> for MouseButton {
    fn from(value: u32) -> Self {
        match value {
            gtk::gdk::BUTTON_PRIMARY => MouseButton::Primary,
            gtk::gdk::BUTTON_MIDDLE => MouseButton::Middle,
            gtk::gdk::BUTTON_SECONDARY => MouseButton::Secondary,
            _ => MouseButton::Primary,
        }
    }
}

impl InputEvent {
    fn handle_event_mouse_input(&mut self, renderer: &FemtoVGArea) -> Option<ToolUpdateResult> {
        if let InputEvent::Mouse(me) = self {
            match me.type_ {
                MouseEventType::Click => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::Release => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::BeginDrag => {
                    me.pos = renderer.abs_canvas_to_image_coordinates(me.pos);
                    None
                }
                MouseEventType::EndDrag | MouseEventType::UpdateDrag => {
                    me.pos = renderer.rel_canvas_to_image_coordinates(me.pos);
                    None
                }
                _ => None,
            }
        } else {
            None
        }
    }

    fn handle_mouse_event(&mut self, renderer: &FemtoVGArea) -> Option<ToolUpdateResult> {
        if let InputEvent::Mouse(me) = self {
            match me.type_ {
                MouseEventType::Click => {
                    if me.button == MouseButton::Secondary {
                        renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                        None
                    } else {
                        None
                    }
                }
                MouseEventType::EndDrag | MouseEventType::UpdateDrag => {
                    if me.button == MouseButton::Middle {
                        renderer.set_drag_offset(me.pos);
                        renderer.set_is_drag(true);

                        if me.type_ == MouseEventType::EndDrag {
                            renderer.store_last_offset();
                            renderer.set_is_drag(false);
                        }
                        renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                    }
                    None
                }

                MouseEventType::Scroll => {
                    let factor = APP_CONFIG.read().zoom_factor();
                    match me.pos.y {
                        v if v < 0.0 => renderer.set_zoom_scale(factor),
                        v if v > 0.0 => renderer.set_zoom_scale(1f32 / factor),
                        _ => {}
                    }
                    renderer.request_render(&APP_CONFIG.read().actions_on_right_click());
                    None
                }
                MouseEventType::PointerPos => {
                    renderer.set_pointer_offset(me.pos);
                    None
                }
                _ => None,
            }
        } else {
            None
        }
    }
}

pub struct SketchBoard {
    renderer: FemtoVGArea,
    active_tool: Rc<RefCell<dyn Tool>>,
    tools: ToolsManager,
    style: Style,
    im_context: gtk::IMMulticontext,
}

struct ImageDataSendable {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl SketchBoard {
    fn refresh_screen(&mut self) {
        self.renderer.queue_render();
    }

    fn image_to_pixbuf(image: RenderedImage) -> Pixbuf {
        let (buf, w, h) = image.into_contiguous_buf();

        Pixbuf::from_bytes(
            &Bytes::from(buf.as_bytes()),
            Colorspace::Rgb,
            true,
            8,
            w as i32,
            h as i32,
            w as i32 * 4,
        )
    }

    fn deactivate_active_tool(&mut self) -> bool {
        if self.active_tool.borrow().active() {
            if let ToolUpdateResult::Commit(result) =
                self.active_tool.borrow_mut().handle_deactivated()
            {
                self.renderer.commit(result);
                return true;
            }
        }
        false
    }

    fn handle_action(&mut self, actions: &[Action]) -> ToolUpdateResult {
        let rv = if self.deactivate_active_tool() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        };
        self.renderer.request_render(actions);
        rv
    }

    fn handle_render_result(
        &self, 
        image: RenderedImage, 
        actions: Vec<Action>, 
        sender: ComponentSender<Self>
    ) {
        let (buf, w, h) = image.into_contiguous_buf();
        let raw_data = buf.as_bytes().to_vec();
        
        let image_data = ImageDataSendable {
            width: w as u32,
            height: h as u32,
            data: raw_data,
        };

        for action in actions {
            match action {
                Action::SaveToClipboard => {
                    self.handle_copy_clipboard(image_data.width, image_data.height, image_data.data.clone());
                }
                Action::SaveToFile => {
                    self.handle_save(image_data.width, image_data.height, image_data.data.clone());
                }
                Action::SaveToFileAs => {
                    let bytes = Bytes::from(&image_data.data);
                    let pixbuf = Pixbuf::from_bytes(
                        &bytes,
                        Colorspace::Rgb,
                        true,
                        8,
                        image_data.width as i32,
                        image_data.height as i32,
                        (image_data.width * 4) as i32,
                    );
                    self.handle_save_as(&pixbuf);
                }
                _ => (),
            }

            if APP_CONFIG.read().early_exit() || action == Action::Exit {
                sender.output_sender().emit(SketchBoardOutput::Exit);
                return;
            }
        }
    }

    fn handle_save(&self, width: u32, height: u32, data: Vec<u8>) {
        let mut output_filename = match APP_CONFIG.read().output_filename() {
            None => {
                println!("No Output filename specified!");
                return;
            }
            Some(o) => o.clone(),
        };

        let delayed_format = chrono::Local::now().format(&output_filename);
        let result = panic::catch_unwind(|| {
            delayed_format.to_string();
        });

        if result.is_err() {
            println!("Warning: chrono format error");
        } else {
            output_filename = format!("{delayed_format}");
        }

        if let Some(tilde_stripped) = output_filename.strip_prefix(&format!("~{}", std::path::MAIN_SEPARATOR_STR)) {
            if let Some(mut p) = std::env::home_dir() {
                p.push(tilde_stripped);
                output_filename = p.to_string_lossy().into_owned();
            }
        }

        thread::spawn(move || {
            let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = 
                ImageBuffer::from_raw(width, height, data).unwrap();
            
            let mut png_data = Vec::new();
            let mut cursor = std::io::Cursor::new(&mut png_data);
            
            if let Err(e) = buffer.write_to(&mut cursor, image::ImageFormat::Png) {
                 // ИСПРАВЛЕНИЕ: используем idle_add_once (глобальный), а не local
                 glib::idle_add_once(move || {
                    log_result(&format!("Error encoding PNG: {e}"), !APP_CONFIG.read().disable_notifications());
                });
                return;
            }

            if output_filename == "-" {
                let stdout = io::stdout();
                let mut handle = stdout.lock();
                if let Err(e) = handle.write_all(&png_data) {
                    eprintln!("Error writing image to stdout: {e}");
                }
            } else {
                match fs::write(&output_filename, png_data) {
                    Ok(_) => {
                        // ИСПРАВЛЕНИЕ: используем idle_add_once
                        glib::idle_add_once(move || {
                            log_result(
                                &format!("File saved to '{}'.", &output_filename),
                                !APP_CONFIG.read().disable_notifications(),
                            );
                        });
                    },
                    Err(e) => {
                        // ИСПРАВЛЕНИЕ: используем idle_add_once
                        glib::idle_add_once(move || {
                             log_result(
                                &format!("Error while saving file: {e}"),
                                !APP_CONFIG.read().disable_notifications(),
                            );
                        });
                    }
                }
            }
        });
    }

    fn handle_save_as(&self, image: &Pixbuf) {
        let data = match image.save_to_bufferv("png", &Vec::new()) {
            Ok(d) => d,
            Err(e) => {
                println!("Error serializing image: {e}");
                return;
            }
        };

        let root = self.renderer.toplevel_window();
        let data = data.clone(); 

        relm4::spawn_local(async move {
            let builder = gtk::FileChooserDialog::builder()
                .modal(false)
                .title("Save Image As")
                .action(gtk::FileChooserAction::Save);

            let dialog = match root {
                Some(w) => builder.transient_for(&w),
                None => builder,
            }
            .build();

            dialog.add_buttons(&[
                ("Cancel", gtk::ResponseType::Cancel),
                ("Save", gtk::ResponseType::Accept),
            ]);

            dialog.connect_response(move |dialog, response| {
                if response == gtk::ResponseType::Accept {
                    if let Some(file) = dialog.file() {
                        let output_filename = match file.path() {
                            Some(path) => path.to_string_lossy().into_owned(),
                            None => return,
                        };

                        match fs::write(&output_filename, &data) {
                            Err(e) => log_result(
                                &format!("Error while saving file: {e}"),
                                !APP_CONFIG.read().disable_notifications(),
                            ),
                            Ok(_) => log_result(
                                &format!("File saved to '{}'.", &output_filename),
                                !APP_CONFIG.read().disable_notifications(),
                            ),
                        };
                    }
                }
                dialog.close();
            });

            dialog.show();
        });
    }

    fn handle_copy_clipboard(&self, width: u32, height: u32, data: Vec<u8>) {
        let copy_command = APP_CONFIG.read().copy_command().cloned();
        
        if let Some(command) = copy_command {
            thread::spawn(move || {
                let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = 
                    ImageBuffer::from_raw(width, height, data.clone()).unwrap();
                
                let mut png_data = Vec::new();
                let mut cursor = std::io::Cursor::new(&mut png_data);
                
                if let Err(e) = buffer.write_to(&mut cursor, image::ImageFormat::Png) {
                    eprintln!("Error encoding png for clipboard: {}", e);
                    return;
                }

                let result = (|| -> anyhow::Result<()> {
                    let mut child = Command::new("sh")
                        .arg("-c")
                        .arg(&command)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .spawn()?;

                    let child_stdin = child.stdin.as_mut().unwrap();
                    child_stdin.write_all(&png_data)?;

                    if !child.wait()?.success() {
                        return Err(anyhow!("Writing to process '{command}' failed."));
                    }
                    Ok(())
                })();

                // ИСПРАВЛЕНИЕ: используем idle_add_once
                glib::idle_add_once(move || {
                     match result {
                        Err(e) => println!("Error saving {e}"),
                        Ok(()) => {
                            log_result(
                                "Copied to clipboard.",
                                !APP_CONFIG.read().disable_notifications(),
                            );
                        }
                    }
                });
            });
        } else {
            let bytes = Bytes::from(&data);
            let pixbuf = Pixbuf::from_bytes(
                &bytes,
                Colorspace::Rgb,
                true,
                8,
                width as i32,
                height as i32,
                (width * 4) as i32,
            );
            let texture = Texture::for_pixbuf(&pixbuf);
            
            let display = DisplayManager::get().default_display();
             if let Some(display) = display {
                display.clipboard().set_texture(&texture);
                log_result(
                    "Copied to clipboard (GTK).",
                    !APP_CONFIG.read().disable_notifications(),
                );
             }
        }
    }

    // ... (Остальной код методов handle_undo, handle_redo, update, init без изменений) ...
    // Вставь сюда остаток файла, который был в прошлый раз (от handle_undo и до конца),
    // он не менялся, кроме init и update, которые уже есть выше.
    // Если ты копируешь по кускам, вот недостающая часть:

    fn handle_undo(&mut self) -> ToolUpdateResult {
        if self.active_tool.borrow().active() {
            self.active_tool.borrow_mut().handle_undo()
        } else if self.renderer.undo() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_redo(&mut self) -> ToolUpdateResult {
        if self.active_tool.borrow().active() {
            self.active_tool.borrow_mut().handle_redo()
        } else if self.renderer.redo() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_reset(&mut self) -> ToolUpdateResult {
        if self.deactivate_active_tool() | self.renderer.reset() {
            ToolUpdateResult::Redraw
        } else {
            ToolUpdateResult::Unmodified
        }
    }

    fn handle_resize(&mut self) -> ToolUpdateResult {
        self.renderer.reset_size(0.);
        self.renderer
            .request_render(&APP_CONFIG.read().actions_on_right_click());
        ToolUpdateResult::Unmodified
    }

    fn handle_original_scale(&mut self) -> ToolUpdateResult {
        self.renderer.reset_size(1.);
        self.renderer
            .request_render(&APP_CONFIG.read().actions_on_right_click());
        ToolUpdateResult::Unmodified
    }

    fn handle_toggle_toolbars_display(
        &mut self,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        sender
            .output_sender()
            .emit(SketchBoardOutput::ToggleToolbarsDisplay);
        ToolUpdateResult::Unmodified
    }

    fn handle_toolbar_event(
        &mut self,
        toolbar_event: ToolbarEvent,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        match toolbar_event {
            ToolbarEvent::ToolSelected(tool) => {
                let old_tool = self.active_tool.clone();
                let mut deactivate_result =
                    old_tool.borrow_mut().handle_event(ToolEvent::Deactivated);

                old_tool.borrow_mut().set_im_context(None);

                if let ToolUpdateResult::Commit(d) = deactivate_result {
                    self.renderer.commit(d);
                    deactivate_result = ToolUpdateResult::Redraw;
                }

                self.active_tool = self.tools.get(&tool);
                self.renderer.set_active_tool(self.active_tool.clone());
                let widget_ref: gtk::Widget = self.renderer.clone().upcast();
                self.active_tool
                    .borrow_mut()
                    .set_im_context(Some(crate::tools::InputContext {
                        im_context: self.im_context.clone(),
                        widget: widget_ref,
                    }));

                self.active_tool
                    .borrow_mut()
                    .set_sender(sender.input_sender().clone());

                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style));

                let activate_result = self
                    .active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::Activated);

                match activate_result {
                    ToolUpdateResult::Unmodified => deactivate_result,
                    _ => activate_result,
                }
            }
            ToolbarEvent::ColorSelected(color) => {
                self.style.color = color;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::SizeSelected(size) => {
                self.style.size = size;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::SaveFile => self.handle_action(&[Action::SaveToFile]),
            ToolbarEvent::CopyClipboard => self.handle_action(&[Action::SaveToClipboard]),
            ToolbarEvent::Undo => self.handle_undo(),
            ToolbarEvent::Redo => self.handle_redo(),
            ToolbarEvent::Reset => self.handle_reset(),
            ToolbarEvent::ToggleFill => {
                self.style.fill = !self.style.fill;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::AnnotationSizeChanged(value) => {
                self.style.annotation_size_factor = value;
                self.active_tool
                    .borrow_mut()
                    .handle_event(ToolEvent::StyleChanged(self.style))
            }
            ToolbarEvent::SaveFileAs => self.handle_action(&[Action::SaveToFileAs]),
            ToolbarEvent::Resize => self.handle_resize(),
            ToolbarEvent::OriginalScale => self.handle_original_scale(),
        }
    }

    fn handle_text_commit(
        &self,
        event: TextEventMsg,
        sender: ComponentSender<Self>,
    ) -> ToolUpdateResult {
        match event {
            TextEventMsg::Commit(txt) => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::Commit(
                        txt.to_string(),
                    )));
                } else if let Some(tool) = txt
                    .chars()
                    .next()
                    .and_then(|char| APP_CONFIG.read().keybinds().get_tool(char))
                {
                    sender.input(SketchBoardInput::ToolbarEvent(ToolbarEvent::ToolSelected(
                        tool,
                    )));
                    sender
                        .output_sender()
                        .emit(SketchBoardOutput::ToolSwitchShortcut(tool));
                } else if let Some(hotkey_digit) =
                    txt.chars().next().and_then(|char| char.to_digit(10))
                {
                    let index_digit = if hotkey_digit == 0 {
                        9
                    } else {
                        hotkey_digit - 1
                    };
                    if APP_CONFIG.read().color_palette().palette().len()
                        >= (index_digit + 1) as usize
                    {
                        sender
                            .output_sender()
                            .emit(SketchBoardOutput::ColorSwitchShortcut(index_digit as u64));
                    }
                }
            }
            TextEventMsg::Preedit {
                text,
                cursor_chars,
                spans,
            } => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::Preedit {
                        text,
                        cursor_chars,
                        spans,
                    }));
                }
            }
            TextEventMsg::PreeditEnd => {
                if self.active_tool_type() == Tools::Text
                    && self.active_tool.borrow().input_enabled()
                {
                    sender.input(SketchBoardInput::new_text_event(TextEventMsg::PreeditEnd));
                }
            }
        }
        ToolUpdateResult::Unmodified
    }

    pub fn active_tool_type(&self) -> Tools {
        self.active_tool.borrow().get_tool_type()
    }
}

// ... и код с реализацией Component и KeyEventMsg, который был в прошлом ответе ...
#[relm4::component(pub)]
impl Component for SketchBoard {
    // Вставь содержимое из прошлого ответа, оно не менялось (кроме update/init которые я обновил выше)
    // Но для надежности скопируй весь блок view, update, init из прошлого ответа
    // только убедись что update зовет self.handle_render_result(..., sender);
    
    // В данном случае я просто повторю концовку для целого файла:
    type CommandOutput = ();
    type Input = SketchBoardInput;
    type Output = SketchBoardOutput;
    type Init = Option<Pixbuf>;

    view! {
        gtk::Box {
            #[local_ref]
            area -> FemtoVGArea {
                set_vexpand: true,
                set_hexpand: true,
                set_can_focus: true,
                set_focusable: true,
                grab_focus: (),

                add_controller = gtk::GestureDrag {
                        set_button: 0,
                        connect_drag_begin[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::BeginDrag,
                                controller.current_button(),
                                1,
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32),
                                false,
                            ));

                        },
                        connect_drag_update[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::UpdateDrag,
                                controller.current_button(),
                                1,
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32),
                                false,
                            ));
                        },
                        connect_drag_end[sender] => move |controller, x, y| {
                            sender.input(SketchBoardInput::new_mouse_event(
                                MouseEventType::EndDrag,
                                controller.current_button(),
                                1,
                                controller.current_event_state(),
                                Vec2D::new(x as f32, y as f32),
                                false
                            ));
                        }
                },

                add_controller = gtk::GestureClick {
                    set_button: 0,
                    connect_pressed[sender] => move |controller, n_pressed, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::Click,
                            controller.current_button(),
                            n_pressed,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32),
                            false,
                        ));
                    },
                    connect_released[sender] => move |controller, n_released, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::Release,
                            controller.current_button(),
                            n_released,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32),
                            true,
                        ));
                    }
                },

                add_controller = gtk::EventControllerScroll{
                    set_flags: gtk::EventControllerScrollFlags::VERTICAL,
                    connect_scroll[sender] => move |_, _, dy| {
                        sender.input(SketchBoardInput::new_scroll_event(dy));
                        glib::Propagation::Stop
                    },
                },

                add_controller = gtk::EventControllerScroll{
                    set_flags: gtk::EventControllerScrollFlags::VERTICAL,
                    connect_scroll[sender] => move |_, _, dy| {
                        sender.input(SketchBoardInput::new_scroll_event(dy));
                        glib::Propagation::Stop
                    },
                },

                add_controller = gtk::EventControllerKey {
                    connect_key_pressed[sender] => move |controller, key, code, modifier | {
                        if let Some(im_context) = controller.im_context() {
                            im_context.focus_in();
                            if !im_context.filter_keypress(controller.current_event().unwrap()) {
                                sender.input(SketchBoardInput::new_key_event(KeyEventMsg::new(key, code, modifier)));
                            }
                        } else {
                            sender.input(SketchBoardInput::new_key_event(KeyEventMsg::new(key, code, modifier)));
                        }
                        glib::Propagation::Stop
                    },

                    connect_key_released[sender] => move |controller, key, code, modifier | {
                        if let Some(im_context) = controller.im_context() {
                            im_context.focus_in();
                            if !im_context.filter_keypress(controller.current_event().unwrap()) {
                                sender.input(SketchBoardInput::new_key_release_event(KeyEventMsg::new(key, code, modifier)));
                            }
                        } else {
                            sender.input(SketchBoardInput::new_key_release_event(KeyEventMsg::new(key, code, modifier)));
                        }
                    },
                    set_im_context: Some(&model.im_context),
                },

                add_controller = gtk::EventControllerMotion {
                    connect_motion[sender] => move |controller, x, y| {
                        sender.input(SketchBoardInput::new_mouse_event(
                            MouseEventType::PointerPos,
                            0,
                            0,
                            controller.current_event_state(),
                            Vec2D::new(x as f32, y as f32),
                            false
                        ));
                    }
                }
            }
        },
    }

    fn update(&mut self, msg: SketchBoardInput, sender: ComponentSender<Self>, _root: &Self::Root) {
        let result = match msg {
             SketchBoardInput::LoadImage(image) => {
                self.renderer.init(
                    sender.input_sender().clone(),
                    self.tools.get_crop_tool(),
                    self.active_tool.clone(),
                    image,
                );
                ToolUpdateResult::Redraw
            }
            SketchBoardInput::InputEvent(mut ie) => {
                if let InputEvent::Key(ke) = ie {
                    let active_tool_result = self
                        .active_tool
                        .borrow_mut()
                        .handle_event(ToolEvent::Input(ie.clone()));

                    match active_tool_result {
                        ToolUpdateResult::StopPropagation
                        | ToolUpdateResult::RedrawAndStopPropagation => active_tool_result,
                        _ => {
                            if ke.is_one_of(Key::z, KeyMappingId::UsZ)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_undo()
                            } else if ke.is_one_of(Key::y, KeyMappingId::UsY)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_redo()
                            } else if ke.is_one_of(Key::t, KeyMappingId::UsT)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.handle_toggle_toolbars_display(sender)
                            } else if ke.is_one_of(Key::s, KeyMappingId::UsS)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.renderer.request_render(&[Action::SaveToFile]);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::s, KeyMappingId::UsS)
                                && ke.modifier
                                    == (ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK)
                            {
                                self.renderer.request_render(&[Action::SaveToFileAs]);
                                ToolUpdateResult::Unmodified
                            } else if ke.is_one_of(Key::c, KeyMappingId::UsC)
                                && ke.modifier == ModifierType::CONTROL_MASK
                            {
                                self.renderer.request_render(&[Action::SaveToClipboard]);
                                ToolUpdateResult::Unmodified
                            } else if (ke.is_one_of(Key::leftarrow, KeyMappingId::ArrowLeft)
                                || ke.is_one_of(Key::rightarrow, KeyMappingId::ArrowRight)
                                || ke.is_one_of(Key::uparrow, KeyMappingId::ArrowUp)
                                || ke.is_one_of(Key::downarrow, KeyMappingId::ArrowDown))
                                && ke.modifier == ModifierType::ALT_MASK
                            {
                                let pan_step_size = APP_CONFIG.read().pan_step_size();
                                match ke.key {
                                    Key::Left => self
                                        .renderer
                                        .set_drag_offset(Vec2D::new(-pan_step_size, 0.)),
                                    Key::Right => {
                                        self.renderer.set_drag_offset(Vec2D::new(pan_step_size, 0.))
                                    }
                                    Key::Up => self
                                        .renderer
                                        .set_drag_offset(Vec2D::new(0., -pan_step_size)),
                                    Key::Down => {
                                        self.renderer.set_drag_offset(Vec2D::new(0., pan_step_size))
                                    }
                                    _ => { /* unreachable */ }
                                }

                                self.renderer.store_last_offset();
                                self.renderer
                                    .request_render(&APP_CONFIG.read().actions_on_right_click());
                                ToolUpdateResult::Unmodified
                            } else if ke.modifier.is_empty() && ke.key == Key::Delete {
                                self.handle_reset()
                            } else if ke.modifier.is_empty()
                                && (ke.key == Key::Escape
                                    || ke.key == Key::Return
                                    || ke.key == Key::KP_Enter)
                            {
                                if let ToolUpdateResult::Unmodified = active_tool_result {
                                    let actions = if ke.key == Key::Escape {
                                        APP_CONFIG.read().actions_on_escape()
                                    } else {
                                        APP_CONFIG.read().actions_on_enter()
                                    };
                                    self.renderer.request_render(&actions);
                                };
                                active_tool_result
                            } else {
                                active_tool_result
                            }
                        }
                    }
                } else {
                    ie.handle_event_mouse_input(&self.renderer);
                    let active_tool_result = self
                        .active_tool
                        .borrow_mut()
                        .handle_event(ToolEvent::Input(ie.clone()));

                    match active_tool_result {
                        ToolUpdateResult::StopPropagation
                        | ToolUpdateResult::RedrawAndStopPropagation => active_tool_result,
                        _ => {
                            if let Some(result) = ie.handle_mouse_event(&self.renderer) {
                                result
                            } else {
                                active_tool_result
                            }
                        }
                    }
                }
            }
            SketchBoardInput::ToolbarEvent(toolbar_event) => {
                self.handle_toolbar_event(toolbar_event, sender)
            }
            SketchBoardInput::RenderResult(img, action) => {
                // Передаем sender для выхода
                self.handle_render_result(img, action, sender);
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::CommitEvent(txt) => {
                self.handle_text_commit(txt, sender);
                ToolUpdateResult::Unmodified
            }
            SketchBoardInput::Refresh => ToolUpdateResult::Redraw,
        };

        match result {
            ToolUpdateResult::Commit(drawable) => {
                self.renderer.commit(drawable);
                self.refresh_screen();
            }
            ToolUpdateResult::Unmodified | ToolUpdateResult::StopPropagation => (),
            ToolUpdateResult::Redraw | ToolUpdateResult::RedrawAndStopPropagation => {
                self.refresh_screen()
            }
        };
    }

    fn init(
        image_opt: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let config = APP_CONFIG.read();
        let tools = ToolsManager::new();

        let im_context = gtk::IMMulticontext::new();

        let mut model = Self {
            renderer: FemtoVGArea::default(),
            active_tool: tools.get(&config.initial_tool()),
            style: Style::default(),
            tools,
            im_context,
        };
        
        let image = image_opt.unwrap_or_else(|| {
             Pixbuf::new(Colorspace::Rgb, true, 8, 1, 1)
                .expect("Failed to create dummy pixbuf")
        });

        let area = &mut model.renderer;
        area.init(
            sender.input_sender().clone(),
            model.tools.get_crop_tool(),
            model.active_tool.clone(),
            image,
        );

        let widgets = view_output!();

        model.im_context.set_client_widget(Some(&model.renderer));
        model.im_context.set_use_preedit(true);

        if let Ok(module) = std::env::var("GTK_IM_MODULE") {
            if module.eq_ignore_ascii_case("fcitx") || module.eq_ignore_ascii_case("fcitx5") {
                model.im_context.set_context_id(Some("fcitx"));
            }
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_commit(move |_cx, txt| {
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::Commit(
                    txt.to_string(),
                )));
            });
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_preedit_changed(move |cx| {
                let (text, attrs, cursor) = cx.preedit_string();
                let cursor_chars = if cursor >= 0 {
                    Some(cursor as usize)
                } else {
                    None
                };
                let spans = spans_from_pango_attrs(text.as_str(), Some(attrs));
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::Preedit {
                    text: text.to_string(),
                    cursor_chars,
                    spans,
                }));
            });
        }

        {
            let sender = sender.input_sender().clone();
            model.im_context.connect_preedit_end(move |_cx| {
                sender.emit(SketchBoardInput::new_commit_event(TextEventMsg::PreeditEnd));
            });
        }

        let focus_controller = gtk::EventControllerFocus::new();
        {
            let im_context = model.im_context.clone();
            focus_controller.connect_enter(move |_| {
                im_context.focus_in();
            });
        }
        {
            let im_context = model.im_context.clone();
            focus_controller.connect_leave(move |_| {
                im_context.focus_out();
            });
        }
        model.renderer.add_controller(focus_controller);

        let widget_ref: gtk::Widget = model.renderer.clone().upcast();
        model
            .active_tool
            .borrow_mut()
            .set_im_context(Some(crate::tools::InputContext {
                im_context: model.im_context.clone(),
                widget: widget_ref,
            }));

        ComponentParts { model, widgets }
    }
}

impl KeyEventMsg {
    pub fn new(key: Key, code: u32, modifier: ModifierType) -> Self {
        Self {
            key,
            code,
            modifier,
        }
    }

    fn is_one_of(&self, key: Key, code: KeyMappingId) -> bool {
        let keymap = KeyMap::from(code);
        self.key == key || self.code as u16 - 8 == keymap.evdev
    }
}