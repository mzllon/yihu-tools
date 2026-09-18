// 一呼面板定位扩展：把一呼面板（tools.yihu.Panel）摆到
// 当前显示器「水平居中、垂直上 1/4 处」并置顶。
//
// Wayland 客户端无自我定位接口，此能力只能由合成器一侧（Shell 扩展）提供。
// 摆放时机：常驻监听「窗口创建 / 尺寸变化」，面板窗口一出现（首帧绘制前）
// 即摆放到位，避免「先系统位置、后跳转」的闪烁；DBus PlaceTop 保留为手动触发。

import Gio from 'gi://Gio';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const IFACE = `
<node>
  <interface name="tools.yihu.ShellPlacer">
    <method name="PlaceTop"/>
  </interface>
</node>`;

const WM_CLASSES = ['tools.yihu.Panel', 'yihu-panel'];

// GNOME 50 起 main.js 不再提供 default 导出，改用命名导出；
// 兼容旧版（default 导出对象）与新版（命名导出）
const lm = Main.layoutManager ?? Main.default?.layoutManager;

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
        // 常驻监听：新窗口创建即尝试摆放（Wayland 下 app_id 稍晚到达，
        // 同时挂 wm-class / size-changed 通知直到确认匹配）
        this._createdId = global.display.connect('window-created', (_d, mw) => {
            this._watch(mw);
        });
        // 扩展热启用场景：已存在的窗口也纳入
        global.get_window_actors().forEach((a) => this._watch(a.meta_window));
    }

    PlaceTop() {
        this._placeAll();
    }

    disable() {
        if (this._createdId) {
            global.display.disconnect(this._createdId);
            this._createdId = 0;
        }
        if (this._ownerId !== undefined) {
            Gio.bus_unown_name(this._ownerId);
            this._ownerId = undefined;
        }
        if (this._exported !== undefined) {
            this._exported.unexport();
            this._exported = undefined;
        }
    }

    _watch(mw) {
        const tryPlace = () => {
            const cls = mw.get_wm_class();
            if (cls && WM_CLASSES.includes(cls)) {
                this._place(mw);
                return true;
            }
            return false;
        };
        if (tryPlace()) {
            return;
        }
        // app_id / 尺寸稍晚才到达：挂通知，确认匹配后自毁这些监听
        const ids = [
            mw.connect('notify::wm-class', () => {
                if (tryPlace()) {
                    ids.forEach((id) => {
                        try {
                            mw.disconnect(id);
                        } catch {}
                    });
                }
            }),
            mw.connect('size-changed', () => {
                if (tryPlace()) {
                    this._place(mw);
                    ids.forEach((id) => {
                        try {
                            mw.disconnect(id);
                        } catch {}
                    });
                } else {
                    this._place(mw);
                }
            }),
        ];
    }

    _place(mw) {
        const cls = mw.get_wm_class();
        if (!cls || !WM_CLASSES.includes(cls)) {
            return;
        }
        const focus = global.display.get_focus_window();
        const index = focus ? focus.get_monitor() : global.display.get_primary_monitor();
        const mon = lm.monitors[index] ?? lm.primaryMonitor;
        const frame = mw.get_frame_rect();
        const w = frame.width > 0 ? frame.width : 720;
        const h = frame.height > 0 ? frame.height : 300;
        const x = mon.x + Math.round((mon.width - w) / 2);
        const y = mon.y + Math.round((mon.height - h) / 4);
        mw.move_frame(true, x, y);
        if (!mw.is_above()) {
            mw.make_above();
        }
    }

    _placeAll() {
        global
            .get_window_actors()
            .filter((a) => WM_CLASSES.includes(a.meta_window.get_wm_class()))
            .forEach((a) => this._place(a.meta_window));
    }
}
