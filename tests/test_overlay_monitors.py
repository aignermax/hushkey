"""Monitor selection uses desktop coordinates, not the primary screen's size."""
import os
import sys
from types import SimpleNamespace

import pytest

import tray


@pytest.mark.parametrize('point, expected', [
    ((-1800, 200), (-1920, 0, 1920, 1080)),
    ((100, -900), (0, -1200, 1920, 1200)),
    ((0, 0), (0, 0, 2560, 1440)),
    ((2559, 1439), (0, 0, 2560, 1440)),
    ((-20, -50), (0, -1200, 1920, 1200)),
])
def test_monitor_at_handles_negative_origins_and_gaps(point, expected):
    monitors = [(-1920, 0, 1920, 1080), (0, -1200, 1920, 1200),
                (0, 0, 2560, 1440)]
    assert tray.monitor_at(point, monitors) == expected


def test_unavailable_monitor_query_keeps_overlay_visible(monkeypatch):
    root = SimpleNamespace(winfo_screenwidth=lambda: 1920,
                           winfo_screenheight=lambda: 1080)
    monkeypatch.setattr(tray.sys, 'platform', 'win32')
    monkeypatch.setattr(tray, 'windows_pointer_monitor',
                        lambda: (_ for _ in ()).throw(OSError('unavailable')),
                        raising=False)
    logged = []
    monkeypatch.setattr(tray.dictate, 'log', logged.append)
    locator = tray.OverlayMonitor(root)
    assert locator.bounds() == (0, 0, 1920, 1080)
    assert locator.bounds() == (0, 0, 1920, 1080)
    assert len(logged) == 1  # polling must not flood the dictation log


def test_transient_query_failure_keeps_last_known_monitor(monkeypatch):
    root = SimpleNamespace(winfo_screenwidth=lambda: 5760,
                           winfo_screenheight=lambda: 2160)
    monkeypatch.setattr(tray.sys, 'platform', 'win32')
    answers = iter([(3840, 0, 1920, 1080), None, (0, 1080, 1920, 1080)])
    def query():
        area = next(answers)
        if area is None:
            raise OSError('display changing')
        return area
    monkeypatch.setattr(tray, 'windows_pointer_monitor', query)
    monkeypatch.setattr(tray.dictate, 'log', lambda _: None)
    locator = tray.OverlayMonitor(root)
    assert locator.bounds() == (3840, 0, 1920, 1080)
    # The center of the virtual framebuffer (2880, 6) is NOT on a screen.
    assert locator.bounds() == (3840, 0, 1920, 1080)
    assert locator.bounds() == (0, 1080, 1920, 1080)


def rect(x, y, w, h):
    return SimpleNamespace(origin=SimpleNamespace(x=x, y=y),
                           size=SimpleNamespace(width=w, height=h))


def test_mac_uses_logical_points_and_flips_secondary_screen_y(monkeypatch):
    primary = SimpleNamespace(frame=lambda: rect(0, 0, 1512, 982))
    above_left = SimpleNamespace(
        frame=lambda: rect(-1920, 982, 1920, 1080),
        visibleFrame=lambda: rect(-1920, 982, 1920, 1055))
    monkeypatch.setitem(sys.modules, 'AppKit', SimpleNamespace(
        NSScreen=SimpleNamespace(screens=lambda: [primary, above_left]),
        NSEvent=SimpleNamespace(mouseLocation=lambda: SimpleNamespace(x=-500, y=1400))))
    # AppKit has a bottom-left origin; Tk has a top-left origin. No Retina
    # multiplier: both AppKit and Aqua Tk position windows in logical points.
    assert tray.mac_pointer_monitor() == (-1920, -1055, 1920, 1055)


def test_x11_uses_pointer_and_randr_monitor_layout():
    root = SimpleNamespace(
        query_pointer=lambda: SimpleNamespace(same_screen=True, root_x=2100, root_y=100),
        xrandr_get_monitors=lambda **kw: SimpleNamespace(monitors=[
            SimpleNamespace(x=0, y=0, width_in_pixels=1920, height_in_pixels=1080),
            SimpleNamespace(x=1920, y=0, width_in_pixels=2560, height_in_pixels=1440)]))
    display = SimpleNamespace(screen=lambda: SimpleNamespace(root=root))
    assert tray.x11_pointer_monitor(display) == (1920, 0, 2560, 1440)


def test_x11_falls_back_to_xinerama_on_older_server():
    root = SimpleNamespace(
        query_pointer=lambda: SimpleNamespace(same_screen=True, root_x=-500, root_y=100),
        xrandr_get_monitors=lambda **kw: (_ for _ in ()).throw(RuntimeError('RandR < 1.5')))
    display = SimpleNamespace(screen=lambda: SimpleNamespace(root=root),
        xinerama_query_screens=lambda: SimpleNamespace(screens=[
            SimpleNamespace(x=-1920, y=0, width=1920, height=1080),
            SimpleNamespace(x=0, y=0, width=2560, height=1440)]))
    assert tray.x11_pointer_monitor(display) == (-1920, 0, 1920, 1080)


@pytest.mark.skipif(os.environ.get('HUSHKEY_TEST_OVERLAY_UI') != '1',
                    reason='requires a desktop or Xvfb')
def test_native_monitor_query_is_usable():
    """CI calls the actual Win32/AppKit/XRandR backend, not just test doubles."""
    import tkinter as tk
    tray.configure_overlay_dpi()
    root = tk.Tk()
    root.withdraw()
    locator = tray.OverlayMonitor(root)
    try:
        if sys.platform == 'win32':
            bounds = tray.windows_pointer_monitor()
        elif sys.platform == 'darwin':
            bounds = tray.mac_pointer_monitor()
        else:
            from Xlib.display import Display
            display = Display()
            try:
                bounds = tray.x11_pointer_monitor(display)
            finally:
                display.close()
        assert bounds[2] > 0 and bounds[3] > 0
        assert locator.bounds()[2] > 0
    finally:
        locator.close()
        root.destroy()
