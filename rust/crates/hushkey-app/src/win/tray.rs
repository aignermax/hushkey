//! The tray icon: a coloured dot for the state, and a small menu.

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Loading,
    Idle,
    Recording,
    Transcribing,
    Listening,
    Failed,
}

impl Look {
    fn colour(self) -> [u8; 3] {
        match self {
            Look::Loading => [0x2b, 0x6c, 0xb0],
            Look::Idle => [0x9e, 0x9e, 0x9e],
            Look::Recording => [0xe5, 0x3e, 0x3e],
            Look::Transcribing => [0xdd, 0x6b, 0x20],
            Look::Listening => [0x38, 0xa1, 0x69],
            Look::Failed => [0x63, 0x17, 0x1b],
        }
    }
}

/// A filled, anti-aliased dot on a transparent background.
fn dot(colour: [u8; 3]) -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let centre = (SIZE as f32 - 1.0) / 2.0;
    let radius = SIZE as f32 * 0.42;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
            rgba.extend_from_slice(&colour);
            rgba.push((alpha * 255.0) as u8);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("a 32x32 RGBA icon")
}

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    pub listen: CheckMenuItem,
    pub settings: MenuId,
    pub quit: MenuId,
    look: Option<Look>,
}

impl Tray {
    pub fn new(listening: bool) -> Result<Tray, String> {
        let status = MenuItem::new("Loading the model…", false, None);
        let listen = CheckMenuItem::new("Continuous listening", true, listening, None);
        let settings = MenuItem::new("Open settings", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &listen,
            &settings,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("hushkey")
            .with_icon(dot(Look::Loading.colour()))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tray {
            icon,
            settings: settings.id().clone(),
            quit: quit.id().clone(),
            status,
            listen,
            look: Some(Look::Loading),
        })
    }

    /// Show `look` and `text` (menu line and tooltip).
    pub fn show(&mut self, look: Look, text: &str) {
        if self.look != Some(look) {
            let _ = self.icon.set_icon(Some(dot(look.colour())));
            self.look = Some(look);
        }
        self.status.set_text(text);
        // Tooltips are limited to 127 characters on Windows.
        let tooltip: String = format!("hushkey: {text}").chars().take(127).collect();
        let _ = self.icon.set_tooltip(Some(tooltip));
    }
}
