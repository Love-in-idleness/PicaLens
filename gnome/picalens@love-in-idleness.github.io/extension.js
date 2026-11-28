// SPDX-License-Identifier: GPL-2.0-or-later
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';

const APP = 'io.github.LoveInIdleness.PicaLens';
const OBJECT = '/io/github/LoveInIdleness/PicaLens/Gnome';
const LAYOUTS = ['fit', 'fill', 'stretch'];
const INTERFACE = `<node>
  <interface name="${APP}.Gnome1">
    <method name="GetLayouts"><arg type="as" direction="out"/></method>
    <method name="GetActiveAppId"><arg type="s" direction="out"/></method>
    <method name="Start">
      <arg name="layout" type="s" direction="in"/>
      <arg name="labels" type="a{ss}" direction="in"/>
      <arg name="error" type="s" direction="out"/>
    </method>
    <method name="StartActive">
      <arg name="layout" type="s" direction="in"/>
      <arg name="error" type="s" direction="out"/>
    </method>
    <method name="StartWithOptions">
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="labels" type="a{ss}" direction="in"/>
      <arg name="error" type="s" direction="out"/>
    </method>
    <method name="StartActiveWithOptions">
      <arg name="options" type="a{sv}" direction="in"/>
      <arg name="error" type="s" direction="out"/>
    </method>
    <method name="Stop"/>
    <signal name="Finished"><arg name="reason" type="s"/></signal>
  </interface>
</node>`;

function clampInt(value, max) {
    const number = typeof value === 'number' && Number.isFinite(value) ? Math.trunc(value) : 0;
    return Math.max(0, Math.min(max, number));
}

function parseOptions(raw) {
    const get = key => {
        const entry = raw?.[key];
        return entry instanceof GLib.Variant ? entry.deepUnpack() : entry;
    };
    return {
        layout: typeof get('layout') === 'string' ? get('layout') : 'fit',
        crop: {
            left: clampInt(get('cropLeft'), 4096),
            top: clampInt(get('cropTop'), 4096),
            right: clampInt(get('cropRight'), 4096),
            bottom: clampInt(get('cropBottom'), 4096),
        },
        hideCursorMs: clampInt(get('hideCursorMs'), 60000),
    };
}

function eligible(actor) {
    const win = actor.meta_window;
    const client = win.get_client_type();
    const interactive = client === Meta.WindowClientType.WAYLAND ||
        (client === Meta.WindowClientType.X11 && typeof actor.transform_xwayland_input === 'boolean');
    return actor.is_mapped() &&
        actor.get_parent() === global.window_group &&
        interactive &&
        win.get_window_type() === Meta.WindowType.NORMAL &&
        !win.is_skip_taskbar() && !win.minimized &&
        win.located_on_workspace(global.workspace_manager.get_active_workspace()) &&
        win.get_gtk_application_id() !== APP &&
        // Windows without a GTK application id can still belong to PicaLens.
        win.get_wm_class() !== APP;
}

export default class PicaLensExtension extends Extension {
    enable() {
        this._session = null;
        this._dbus = Gio.DBusExportedObject.wrapJSObject(INTERFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT);
    }

    disable() {
        this._finish('extension disabled');
        this._dbus.unexport();
        this._dbus = null;
    }

    GetLayouts() {
        return LAYOUTS;
    }

    GetActiveAppIdAsync(_parameters, invocation) {
        const focused = global.display.focus_window;
        this._withOwner(invocation, () => {
            const actor = focused?.get_compositor_private();
            const appId = actor && global.display.focus_window === focused && eligible(actor)
                ? (focused.get_gtk_application_id() || focused.get_wm_class() || '')
                : '';
            invocation.return_value(new GLib.Variant('(s)', [appId]));
        });
    }

    StartAsync([layout, labels], invocation) {
        this._requestStart({layout, crop: {left: 0, top: 0, right: 0, bottom: 0}, hideCursorMs: 0}, labels, invocation);
    }

    StartActiveAsync([layout], invocation) {
        this._requestStart({layout, crop: {left: 0, top: 0, right: 0, bottom: 0}, hideCursorMs: 0}, null, invocation);
    }

    StartWithOptionsAsync([options, labels], invocation) {
        this._requestStart(parseOptions(options), labels, invocation);
    }

    StartActiveWithOptionsAsync([options], invocation) {
        this._requestStart(parseOptions(options), null, invocation);
    }

    _withOwner(invocation, handler) {
        const exported = this._dbus;
        const bus = invocation.get_connection();
        const sender = invocation.get_sender();
        // Only the PicaLens owner can inspect or transform windows.
        bus.call('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
            'GetNameOwner', new GLib.Variant('(s)', [APP]), new GLib.VariantType('(s)'),
            Gio.DBusCallFlags.NONE, 1000, null, (connection, result) => {
                let owner;
                try {
                    [owner] = connection.call_finish(result).deepUnpack();
                } catch {
                    owner = null;
                }
                if (sender !== owner) {
                    invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied',
                        'Only the active PicaLens application can start a session');
                    return;
                }
                handler(sender, this._dbus === exported);
            });
    }

    _requestStart(options, labels, invocation) {
        const focused = labels ? null : global.display.focus_window;
        this._withOwner(invocation, (sender, active) => {
            let error;
            try {
                error = active && LAYOUTS.includes(options.layout)
                    ? this._start(sender, options, labels, focused) : 'gnome_unavailable';
            } catch (e) {
                logError(e, 'PicaLens window selection');
                this._finish('gnome_unavailable');
                error = 'gnome_unavailable';
            }
            invocation.return_value(new GLib.Variant('(s)', [error]));
        });
    }

    StopAsync(_parameters, invocation) {
        if (this._session?.owner === invocation.get_sender())
            this._finish('requested stop');
        invocation.return_value(new GLib.Variant('()', []));
    }

    _connect(session, object, signal, callback) {
        session.signals.push([object, object.connect(signal, callback)]);
    }

    _start(owner, options, labels, focused) {
        if (this._session || Main.sessionMode.isLocked || Main.overview.visible)
            return 'gnome_unavailable';

        let actor, actors;
        if (labels) {
            const recent = global.display.get_tab_list(Meta.TabList.NORMAL_ALL,
                global.workspace_manager.get_active_workspace());
            actors = recent.map(win => win.get_compositor_private()).filter(actor => actor && eligible(actor));
            if (!actors.length)
                return 'gnome_no_windows';
        } else {
            actor = focused?.get_compositor_private();
            if (!actor || global.display.focus_window !== focused || !eligible(actor))
                return 'no_active_window';
        }

        const session = {
            owner,
            layout: options.layout,
            crop: options.crop,
            hideCursorMs: options.hideCursorMs,
            signals: [],
        };
        this._session = session;
        session.watcher = Gio.bus_watch_name_on_connection(Gio.DBus.session, owner,
            Gio.BusNameWatcherFlags.NONE, null, () => this._finish('client disconnected'));
        this._connect(session, Main.overview, 'showing', () => this._finish('overview'));
        this._connect(session, Main.sessionMode, 'updated', () => {
            if (Main.sessionMode.isLocked)
                this._finish('session locked');
        });
        this._connect(session, Main.layoutManager, 'monitors-changed', () => this._finish('display changed'));
        this._connect(session, global.workspace_manager, 'active-workspace-changed',
            () => this._finish('workspace changed'));

        if (actor) {
            this._present(session, actor);
            return '';
        }
        return this._choose(session, actors, labels);
    }

    _choose(session, actors, labels) {
        const dialog = new ModalDialog.ModalDialog({styleClass: 'picalens-dialog', destroyOnClose: false});
        session.dialog = dialog;
        const heading = new St.Label({text: labels.heading, style_class: 'prompt-dialog-headline'});
        dialog.contentLayout.add_child(heading);
        dialog.contentLayout.add_child(new St.Label({text: labels.description}));
        const rows = new St.BoxLayout({orientation: Clutter.Orientation.VERTICAL, style: 'spacing: 6px;'});
        const scroll = new St.ScrollView({overlay_scrollbars: true, x_expand: true,
            style: 'min-width: 320px; max-height: 320px;'});
        scroll.set_child(rows);
        dialog.contentLayout.add_child(scroll);
        for (const actor of actors) {
            const title = actor.meta_window.get_title() || labels.untitled;
            const text = new St.Label({text: Array.from(title).slice(0, 70).join(''), x_expand: true});
            const row = new St.Button({child: text, accessible_name: title, style_class: 'button',
                can_focus: true, x_expand: true, x_align: Clutter.ActorAlign.FILL});
            row.connect('clicked', () => {
                session.selected = actor;
                dialog.close();
            });
            rows.add_child(row);
        }
        dialog.setButtons([{label: labels.cancel, key: Clutter.KEY_Escape, action: () => dialog.close()}]);
        this._connect(session, dialog, 'closed', () => {
            if (session.selected)
                this._activate(session);
            else
                this._finish('selection cancelled');
        });
        if (dialog.open())
            return '';
        this._finish('selection unavailable');
        return 'gnome_unavailable';
    }

    _activate(session) {
        const actor = session.selected;
        if (!eligible(actor)) {
            this._finish('gnome_source_lost');
            return;
        }
        const win = actor.meta_window;
        Main.activateWindow(win);
        // The Shell modal releases its grab before returning keyboard focus.
        session.activateId = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            session.activateId = 0;
            if (!eligible(actor) || global.display.focus_window !== win) {
                this._finish('gnome_source_lost');
                return GLib.SOURCE_REMOVE;
            }
            try {
                this._present(session, actor);
            } catch (e) {
                logError(e, 'PicaLens presentation');
                this._finish('gnome_unavailable');
            }
            return GLib.SOURCE_REMOVE;
        });
    }

    _present(session, actor) {
        const win = actor.meta_window;
        const index = win.get_monitor();
        const monitor = Main.layoutManager.monitors[index];
        const frame = win.get_frame_rect();
        if (!monitor || frame.width <= 0 || frame.height <= 0) {
            this._finish('gnome_source_lost');
            return;
        }
        const crop = session.crop ?? {left: 0, top: 0, right: 0, bottom: 0};
        const source = {
            x: frame.x + crop.left,
            y: frame.y + crop.top,
            width: frame.width - crop.left - crop.right,
            height: frame.height - crop.top - crop.bottom,
        };
        if (source.width < 16 || source.height < 16) {
            this._finish('invalid_crop');
            return;
        }
        const ratioX = monitor.width / source.width;
        const ratioY = monitor.height / source.height;
        const uniform = session.layout === 'fill' ? Math.max(ratioX, ratioY) : Math.min(ratioX, ratioY);
        const sx = session.layout === 'stretch' ? ratioX : uniform;
        const sy = session.layout === 'stretch' ? ratioY : uniform;
        const x = monitor.x + (monitor.width - source.width * sx) / 2;
        const y = monitor.y + (monitor.height - source.height * sy) / 2;
        const width = Math.min(source.width, monitor.width / sx);
        const height = Math.min(source.height, monitor.height / sy);
        const pivot = actor.get_pivot_point();
        const [scaleX, scaleY] = actor.get_scale();
        const [tx, ty, tz] = actor.get_translation();
        const clip = actor.has_clip ? actor.get_clip() : null;
        const xwayland = win.get_client_type() === Meta.WindowClientType.X11;
        const previousXwaylandInput = xwayland ? actor.transform_xwayland_input : null;
        session.restore = () => {
            if (xwayland)
                actor.transform_xwayland_input = previousXwaylandInput;
            actor.set_pivot_point(...pivot);
            actor.set_scale(scaleX, scaleY);
            actor.set_translation(tx, ty, tz);
            if (clip)
                actor.set_clip(...clip);
            else
                actor.remove_clip();
        };

        const backdrop = new St.Widget({style: 'background-color: black;', reactive: true,
            x: monitor.x, y: monitor.y, width: monitor.width, height: monitor.height});
        session.backdrop = backdrop;
        actor.get_parent().insert_child_below(backdrop, actor);
        if (xwayland) {
            actor.transform_xwayland_input = true;
            // Mutter can send a key straight to XWayland before the Stage sees it.
            session.escapeAction = global.display.grab_accelerator('Escape', Meta.KeyBindingFlags.IGNORE_AUTOREPEAT);
            if (session.escapeAction) {
                Main.wm.allowKeybinding(Meta.external_binding_name_for_action(session.escapeAction),
                    Shell.ActionMode.NORMAL);
                this._connect(session, global.display, 'accelerator-activated', (_display, action) => {
                    if (action === session.escapeAction)
                        this._finish('Escape');
                });
            }
        }
        actor.set_pivot_point(0, 0);
        actor.set_scale(sx, sy);
        actor.set_translation(x - actor.x + (actor.x - source.x) * sx,
            y - actor.y + (actor.y - source.y) * sy, tz);
        actor.set_clip(source.x - actor.x + (source.width - width) / 2,
            source.y - actor.y + (source.height - height) / 2, width, height);

        const cursorTracker = session.hideCursorMs > 0 ? global.backend?.get_cursor_tracker?.() : null;
        if (cursorTracker && typeof cursorTracker.inhibit_cursor_visibility === 'function') {
            session.showCursor = () => {
                if (session.cursorHidden) {
                    session.cursorHidden = false;
                    cursorTracker.uninhibit_cursor_visibility();
                }
            };
            session.armCursorTimer = () => {
                if (session.cursorTimerId) {
                    GLib.source_remove(session.cursorTimerId);
                    session.cursorTimerId = 0;
                }
                session.cursorTimerId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, session.hideCursorMs, () => {
                    session.cursorTimerId = 0;
                    if (this._session === session && !session.cursorHidden) {
                        session.cursorHidden = true;
                        cursorTracker.inhibit_cursor_visibility();
                    }
                    return GLib.SOURCE_REMOVE;
                });
            };
            session.armCursorTimer();
        }

        if (index === Main.layoutManager.primaryIndex && Main.layoutManager.panelBox.visible) {
            session.panel = Main.layoutManager.panelBox;
            session.panel.hide();
        }
        for (const signal of ['unmanaged', 'position-changed', 'size-changed', 'notify::minimized'])
            this._connect(session, win, signal, () => this._finish('source changed'));
        this._connect(session, actor, 'destroy', () => {
            session.restore = null;
            this._finish('source closed');
        });
        this._connect(session, global.display, 'notify::focus-window', () => {
            if (global.display.focus_window !== win)
                this._finish('source lost focus');
        });
        this._connect(session, global.display, 'restacked', () => {
            if (backdrop.get_parent() === actor.get_parent())
                actor.get_parent().set_child_below_sibling(backdrop, actor);
        });
        this._connect(session, global.stage, 'captured-event', (_stage, event) => {
            const type = event.type();
            if (session.armCursorTimer && (type === Clutter.EventType.MOTION ||
                type === Clutter.EventType.BUTTON_PRESS || type === Clutter.EventType.SCROLL ||
                type === Clutter.EventType.TOUCH_BEGIN)) {
                session.showCursor();
                session.armCursorTimer();
            }
            if (type === Clutter.EventType.KEY_PRESS && event.get_key_symbol() === Clutter.KEY_Escape) {
                this._finish('Escape');
                return Clutter.EVENT_STOP;
            }
            if (type !== Clutter.EventType.BUTTON_PRESS && type !== Clutter.EventType.SCROLL &&
                type !== Clutter.EventType.TOUCH_BEGIN)
                return Clutter.EVENT_PROPAGATE;
            const [px, py] = event.get_coords();
            if (px < monitor.x || px >= monitor.x + monitor.width ||
                py < monitor.y || py >= monitor.y + monitor.height)
                return Clutter.EVENT_PROPAGATE;
            const target = global.stage.get_actor_at_pos(Clutter.PickMode.REACTIVE, px, py);
            return target && actor.contains(target) ? Clutter.EVENT_PROPAGATE : Clutter.EVENT_STOP;
        });
        console.log(`PicaLens: interactive ${xwayland ? 'XWayland' : 'Wayland'} magnification started`);
    }

    _finish(reason) {
        const session = this._session;
        if (!session)
            return;
        this._session = null;
        for (const [object, id] of session.signals)
            object.disconnect(id);
        if (session.activateId)
            GLib.source_remove(session.activateId);
        if (session.cursorTimerId)
            GLib.source_remove(session.cursorTimerId);
        session.showCursor?.();
        if (session.watcher)
            Gio.bus_unwatch_name(session.watcher);
        if (session.escapeAction) {
            Main.wm.allowKeybinding(Meta.external_binding_name_for_action(session.escapeAction),
                Shell.ActionMode.NONE);
            global.display.ungrab_accelerator(session.escapeAction);
        }
        try {
            session.dialog?.close();
            session.dialog?.destroy();
            session.restore?.();
        } finally {
            session.backdrop?.destroy();
            session.panel?.show();
            this._dbus?.emit_signal('Finished', new GLib.Variant('(s)', [reason]));
            console.log(`PicaLens: interactive session ended (${reason})`);
        }
    }
}
