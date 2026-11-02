//! The preview pill: a small always-on-top window at the top of the screen
//! under the mouse, showing the state and, while recording, what was heard
//! so far. It never takes the focus, so typing and pasting are unaffected.
//! Looks like the Python app's: dark, a coloured dot, white text.

use std::ptr;
use std::sync::{mpsc, Mutex, Once};

use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW,
    Ellipse, EndPaint, FillRect, GetDC, GetMonitorInfoW, InvalidateRect, MonitorFromPoint,
    ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, CLEARTYPE_QUALITY,
    DEFAULT_CHARSET, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK, FW_NORMAL, HFONT, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, PostMessageW,
    RegisterClassW, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST, MSG, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_HIDE, WM_APP, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

const WM_REFRESH: u32 = WM_APP + 1;
const BACKGROUND: [u8; 3] = [0x2d, 0x35, 0x42];
/// The most text shown; older words scroll out at the front.
const MAX_CHARS: usize = 500;

#[derive(Clone, Default)]
struct Pill {
    /// None: hidden.
    colour: Option<[u8; 3]>,
    text: String,
    /// Text width and height in pixels, as last laid out.
    text_size: (i32, i32),
    scale: f32,
}

static PILL: Mutex<Option<Pill>> = Mutex::new(None);

fn rgb(c: [u8; 3]) -> COLORREF {
    c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub struct Overlay {
    hwnd: usize,
}

impl Overlay {
    pub fn new() -> Result<Overlay, String> {
        *PILL.lock().unwrap() = Some(Pill::default());
        let (tx, rx) = mpsc::channel::<Result<usize, String>>();
        std::thread::Builder::new()
            .name("hushkey-overlay".into())
            .spawn(move || run(tx))
            .map_err(|e| e.to_string())?;
        let hwnd = rx
            .recv()
            .map_err(|_| "the overlay did not start".to_string())??;
        Ok(Overlay { hwnd })
    }

    /// Show the pill with a `colour`ed dot and `text` (the newest part if
    /// long), on the screen under the mouse.
    pub fn show(&self, colour: [u8; 3], text: &str) {
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let count = text.chars().count();
        let text = if count > MAX_CHARS {
            "…".to_string() + &text.chars().skip(count - MAX_CHARS).collect::<String>()
        } else {
            text
        };
        if let Some(pill) = PILL.lock().unwrap().as_mut() {
            pill.colour = Some(colour);
            pill.text = text;
        }
        unsafe { PostMessageW(self.hwnd as HWND, WM_REFRESH, 0, 0) };
    }

    pub fn hide(&self) {
        if let Some(pill) = PILL.lock().unwrap().as_mut() {
            pill.colour = None;
        }
        unsafe { PostMessageW(self.hwnd as HWND, WM_REFRESH, 0, 0) };
    }
}

static CLASS: Once = Once::new();

fn run(ready: mpsc::Sender<Result<usize, String>>) {
    let class = wide("hushkey-rs-overlay");
    unsafe {
        let instance = GetModuleHandleW(ptr::null());
        CLASS.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&wc);
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            wide("hushkey").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            ptr::null(),
        );
        if hwnd.is_null() {
            let _ = ready.send(Err("could not create the overlay window".into()));
            return;
        }
        let _ = ready.send(Ok(hwnd as usize));
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn font(scale: f32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            -(15.0 * scale) as i32, // ~11 pt at 96 dpi
            0,
            0,
            0,
            FW_NORMAL as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            CLEARTYPE_QUALITY as u32,
            0,
            face.as_ptr(),
        )
    }
}

/// Size and place the window for the current pill, or hide it.
unsafe fn refresh(hwnd: HWND) {
    let Some(mut pill) = PILL.lock().unwrap().clone() else {
        return;
    };
    if pill.colour.is_none() {
        ShowWindow(hwnd, SW_HIDE);
        return;
    }
    let mut cursor = POINT { x: 0, y: 0 };
    GetCursorPos(&mut cursor);
    let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(monitor, &mut info);
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
    let scale = dpi_x as f32 / 96.0;
    let work = info.rcWork;
    let work_width = work.right - work.left;
    let px = |v: f32| (v * scale).round() as i32;
    // Measure the text wrapped at the width the Python pill uses.
    let max_text = px(620.0).min(work_width - px(80.0)).max(px(100.0));
    let dc = GetDC(hwnd);
    let font = font(scale);
    let old = SelectObject(dc, font as _);
    let text = wide(&pill.text);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: max_text,
        bottom: 0,
    };
    DrawTextW(
        dc,
        text.as_ptr(),
        -1,
        &mut rect,
        DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
    );
    SelectObject(dc, old);
    DeleteObject(font as _);
    ReleaseDC(hwnd, dc);
    pill.text_size = (rect.right - rect.left, rect.bottom - rect.top);
    pill.scale = scale;
    let width = px(12.0 + 12.0 + 8.0 + 14.0) + pill.text_size.0;
    let height = px(10.0) + pill.text_size.1.max(px(18.0));
    let x = work.left + (work_width - width) / 2;
    let y = work.top + px(6.0);
    if let Some(stored) = PILL.lock().unwrap().as_mut() {
        stored.text_size = pill.text_size;
        stored.scale = scale;
    }
    let region = CreateRoundRectRgn(0, 0, width + 1, height + 1, px(14.0), px(14.0));
    SetWindowRgn(hwnd, region, 1); // the window owns the region now
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        x,
        y,
        width,
        height,
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    InvalidateRect(hwnd, ptr::null(), 1);
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut ps);
    if let Some(pill) = PILL.lock().unwrap().clone() {
        if let Some(colour) = pill.colour {
            let scale = if pill.scale > 0.0 { pill.scale } else { 1.0 };
            let px = |v: f32| (v * scale).round() as i32;
            let background = CreateSolidBrush(rgb(BACKGROUND));
            FillRect(dc, &ps.rcPaint, background);
            DeleteObject(background as _);
            // The dot, vertically centred on the first line.
            let dot = CreateSolidBrush(rgb(colour));
            let old_brush = SelectObject(dc, dot as _);
            let pen = windows_sys::Win32::Graphics::Gdi::GetStockObject(
                windows_sys::Win32::Graphics::Gdi::NULL_PEN,
            );
            let old_pen = SelectObject(dc, pen);
            let (cx, cy, r) = (px(18.0), px(5.0) + px(9.0), px(5.0));
            Ellipse(dc, cx - r, cy - r, cx + r, cy + r);
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            DeleteObject(dot as _);
            let font = font(scale);
            let old_font = SelectObject(dc, font as _);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, rgb([0xff, 0xff, 0xff]));
            let text = wide(&pill.text);
            let left = px(12.0 + 12.0 + 8.0);
            let mut rect = RECT {
                left,
                top: px(5.0),
                right: left + pill.text_size.0,
                bottom: px(5.0) + pill.text_size.1,
            };
            DrawTextW(dc, text.as_ptr(), -1, &mut rect, DT_WORDBREAK | DT_NOPREFIX);
            SelectObject(dc, old_font);
            DeleteObject(font as _);
        }
    }
    EndPaint(hwnd, &ps);
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_REFRESH => {
            refresh(hwnd);
            0
        }
        WM_PAINT => {
            paint(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, GetDIBits, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };

    /// The work area of the monitor under the mouse, and a capture of its
    /// top strip as RGBA.
    unsafe fn capture_top(height: i32) -> (RECT, Vec<u8>) {
        let mut cursor = POINT { x: 0, y: 0 };
        GetCursorPos(&mut cursor);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(
            MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST),
            &mut info,
        );
        let work = info.rcWork;
        let width = work.right - work.left;
        let screen = GetDC(ptr::null_mut());
        let memory = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        let old = SelectObject(memory, bitmap as _);
        BitBlt(
            memory, 0, 0, width, height, screen, work.left, work.top, SRCCOPY,
        );
        SelectObject(memory, old);
        let mut header: BITMAPINFO = std::mem::zeroed();
        header.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bgra = vec![0u8; (width * height * 4) as usize];
        GetDIBits(
            memory,
            bitmap,
            0,
            height as u32,
            bgra.as_mut_ptr().cast(),
            &mut header,
            DIB_RGB_COLORS,
        );
        DeleteObject(bitmap as _);
        DeleteDC(memory);
        ReleaseDC(ptr::null_mut(), screen);
        let rgba = bgra
            .chunks(4)
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        (work, rgba)
    }

    #[test]
    #[ignore = "shows a window on the desktop; run with --ignored"]
    fn the_pill_shows_at_the_top_of_the_screen_under_the_mouse() {
        let overlay = Overlay::new().unwrap();
        overlay.show(
            [0xe5, 0x3e, 0x3e],
            "Hallo, das ist ein Test der Vorschau von hushkey.",
        );
        std::thread::sleep(std::time::Duration::from_millis(600));
        let (work, rgba) = unsafe { capture_top(120) };
        overlay.hide();
        let width = (work.right - work.left) as usize;
        if let Some(path) = std::env::var_os("HUSHKEY_OVERLAY_SHOT") {
            let file = std::fs::File::create(path).unwrap();
            let mut png = png::Encoder::new(file, width as u32, 120);
            png.set_color(png::ColorType::Rgba);
            png.set_depth(png::BitDepth::Eight);
            png.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
        // The dark pill and its red dot are near the top centre.
        let pixel = |x: usize, y: usize| &rgba[(y * width + x) * 4..(y * width + x) * 4 + 3];
        let dark = (0..60).any(|y| (0..width).any(|x| pixel(x, y) == BACKGROUND));
        let red = (0..60).any(|y| (0..width).any(|x| pixel(x, y) == [0xe5, 0x3e, 0x3e]));
        assert!(dark && red, "no pill found (dark {dark}, red {red})");
    }
}
