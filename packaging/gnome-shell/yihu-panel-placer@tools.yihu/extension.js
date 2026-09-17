// 一呼面板定位扩展：监听 DBus 请求，把一呼面板（tools.yihu.Panel）
// 移到当前显示器「水平居中、垂直上 1/4 处」并置顶。
//
// Wayland 客户端无自我定位接口，此能力只能由合成器一侧（Shell 扩展）提供。

import Gio from 'gi://Gio';
import Main from 'resource:///org/gnome/shell/ui/main.js';

const IFACE = `
<node>
  <interface name="tools.yihu.ShellPlacer">
    <method name="PlaceTop"/>
  </interface>
</node>`;

const WM_CLASS = 'tools.yihu.Panel';

export default class YihuPanelPlacerExtension {
    enable() {
        this._ownerId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            'tools.yihu.ShellPlacer',
            Gio.BusNameOwnerFlags.NONE,
            (bus, _name) => {
                this._exported = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
                this._exported.export(bus, '/tools/yihu/ShellPlacer');
            },
            null,
            null
        );
    }

    PlaceTop() {
        const wins = global
            .get_window_actors()
            .filter((a) => a.meta_window.get_wm_class() === WM_CLASS);
        if (wins.length === 0) {
            return;
        }
        const focus = global.display.get_focus_window();
        const index = focus ? focus.get_monitor() : global.display.get_primary_monitor();
        const mon = Main.layoutManager.monitors[index] ?? Main.layoutManager.primaryMonitor;
        for (const actor of wins) {
            const mw = actor.meta_window;
            const frame = mw.get_frame_rect();
            const x = mon.x + Math.round((mon.width - frame.width) / 2);
            const y = mon.y + Math.round((mon.height - frame.height) / 4);
            mw.move_frame(true, x, y);
            if (!mw.is_above()) {
                mw.make_above();
            }
        }
    }

    disable() {
        if (this._ownerId !== undefined) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = undefined;
        }
        if (this._exported !== undefined) {
            this._exported.unexport();
            this._exported = undefined;
        }
    }
}
