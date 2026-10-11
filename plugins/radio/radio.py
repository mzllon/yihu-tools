#!/usr/bin/env python3
"""电台插件（M5 第五批：内置电台功能完全插件化、可插拔）。

真插件形态：独立进程 + bwrap 沙箱（无网无声），resident 常驻。
- 分类/频道/搜索：network.fetch 经宿主代访蜻蜓 FM 公开 Web 接口
  （第三方客户端通行用法，仅限个人收听）；
- 播放/停止/音量：audio.play@host / audio.stop / audio.volume 经宿主
  能力代理代执行（gst-play 子进程 + HLS 清单镜像在宿主侧，收起面板
  音乐不断；守护进程退出即停播）；
- 收藏：插件数据目录 /data/favorites.json（旧中心格式 content_id 兼容
  读取，迁移见 scripts/migrate-radio-favorites.sh）。

UI（模板 v2.1 行内动作按钮）：分类下拉 + 搜索框 + 音量数字框 +
「浏览/搜索/应用音量」动作；频道行带 ▶ 播放 / ★ 收藏按钮，顶部
状态行带 ⏹ 停止。面板输 电台/radio/fm 开窗。
"""

import json
import os
import sys
import urllib.parse

# 蜻蜓 FM 公开 Web 接口（与原中心实现同源）
CATS_URL = "https://rapi.qtfm.cn/categories?type=channel"
PAGE_SIZE = 50


def channels_url(cat):
    return f"https://rapi.qtfm.cn/categories/{cat}/channels?page=1&pagesize={PAGE_SIZE}"


def search_url(kw):
    return "https://search.qingting.fm/v3/search?k=" + urllib.parse.quote(kw, safe="")


def stream_url(cid):
    return f"https://ls.qingting.fm/live/{cid}/64k.m3u8"


FAVORITES_ID = 0  # 「我的收藏」伪分类
FAV_PATH = os.path.join(os.environ.get("HOME", "/data"), "favorites.json")

HINTS = {"radio", "电台", "广播", "fm", "收音机"}

UI_SEQ = 100
_seq = 5000
CATS = None      # [(id, title)]；首次进入经 network.fetch 拉取
CUR_CAT = FAVORITES_ID
CURRENT = None   # {"id","title"} 正在播放
LAST_CH = []     # 最近一次浏览/搜索的频道列表（收藏切换后就地重渲染）
LAST_SRC = "cat" # LAST_CH 的来源：cat=CUR_CAT 分类 / search=搜索结果
PENDING_VOL = [80]   # play 请求携带的待应用音量（列表包装避免 global）
LAST_VOL = [80]      # 最近一次窗内/面板使用的音量
PLAY_FROM_WIN = [False]  # 播放/停止是否来自窗内（决定响应后是否刷窗）
PANEL_QID = [0]      # 最新一次面板搜台的 query id（丢弃过期击键的迟到响应）
PANEL_KW = [""]      # 面板搜台关键字（无结果提示用）


def send(o):
    sys.stdout.write(json.dumps(o, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def err_row(msg):
    return {"title": "✗ " + msg, "subtitle": "稍后重试"}


# ---- 能力编排（同 hotkeys 模式：PENDING 映射回跳） -------------------------

pending = {}  # request_id -> (tag, qid)


def _next_id():
    global _seq
    _seq += 1
    return _seq


def req(tag, qid, capability, params):
    rid = _next_id()
    pending[rid] = (tag, qid)
    send({"type": "capability_request", "id": rid, "capability": capability,
          "params": params})


# ---- 数据解析（与原 radio_api.rs 同字段） ---------------------------------

def parse_cats(text):
    try:
        data = json.loads(text).get("Data") or []
    except Exception:
        return []
    out = []
    for it in data:
        if isinstance(it, dict) and isinstance(it.get("id"), int) and it.get("title"):
            out.append((it["id"], it["title"]))
    return out


def _channel(it):
    cid = it.get("content_id") or it.get("id")
    if not isinstance(cid, int):
        return None
    np = it.get("nowplaying") or {}
    return {
        "id": cid,
        "title": str(it.get("title") or "")[:60],
        "np": str(np.get("name") or np.get("title") or "")[:50],
        "aud": str(it.get("audience_count") or "")[:20],
    }


def parse_channels(text):
    try:
        data = json.loads(text).get("Data") or []
    except Exception:
        return []
    return [c for c in (_channel(it) for it in data if isinstance(it, dict)) if c]


def parse_search(text):
    try:
        docs = (((json.loads(text).get("data") or {}).get("data") or {}).get("docs")) or []
    except Exception:
        return []
    return [c for c in (_channel(d) for d in docs
                        if isinstance(d, dict) and d.get("type") == "channel_live") if c]


# ---- 收藏（/data/favorites.json；旧中心格式 content_id 兼容读取） --------

def load_favs():
    try:
        arr = json.loads(open(FAV_PATH, encoding="utf-8").read())
    except Exception:
        return []
    if not isinstance(arr, list):
        return []
    out = []
    for it in arr:
        if not isinstance(it, dict):
            continue
        cid = it.get("id") or it.get("content_id")
        if isinstance(cid, int) and it.get("title"):
            # 归一化成频道同形（旧中心格式无 nowplaying/audience 字段）
            out.append({"id": cid, "title": str(it["title"])[:60], "np": "", "aud": ""})
    return out


def save_favs(favs):
    tmp = FAV_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(favs, f, ensure_ascii=False)
    os.replace(tmp, FAV_PATH)


# ---- UI 构建 ---------------------------------------------------------------

def channel_rows(channels):
    fav_ids = {f["id"] for f in load_favs()}
    rows = []
    for ch in channels:
        sub = []
        if ch["np"]:
            sub.append("正在播：" + ch["np"])
        if ch["aud"]:
            sub.append(ch["aud"] + " 听众")
        fid = ch["id"]
        if fid in fav_ids:
            fav_btn = {"key": f"unfav:{fid}", "label": "★ 已藏"}
        else:
            fav_btn = {"key": f"fav:{fid}|{ch['title']}", "label": "☆ 收藏"}
        rows.append({
            "title": ch["title"] or f"频道 {fid}",
            "subtitle": " · ".join(sub) or stream_host_note(),
            "actions": [
                {"key": f"play:{fid}|{ch['title']}", "label": "▶ 播放"},
                fav_btn,
            ],
        })
    return rows


def stream_host_note():
    return "点「▶ 播放」收听（经宿主播放）"


def status_rows():
    if CURRENT:
        return [{
            "title": f"🔊 正在播放：{CURRENT['title']}",
            "subtitle": "宿主侧播放 · 收起面板不中断",
            "actions": [{"key": "stop", "label": "⏹ 停止"}],
        }]
    return [{
        "title": "⏹ 未在播放",
        "subtitle": "选台后点「▶ 播放」（经宿主能力代理）",
    }]


def ui_spec(ui_id, rows):
    cats = CATS or []
    choices = [{"label": "我的收藏", "value": str(FAVORITES_ID)}]
    choices += [{"label": t, "value": str(i)} for i, t in cats]
    return {
        "type": "ui.show",
        "ui_id": ui_id,
        "spec": {
            "title": "电台",
            "fields": [
                {"key": "cat", "kind": "select", "label": "分类",
                 "choices": choices, "value": str(CUR_CAT)},
                {"key": "kw", "kind": "text", "label": "搜索电台",
                 "placeholder": "如 交通91.8"},
                {"key": "vol", "kind": "number", "label": "音量（0-100，播放与「应用音量」生效）",
                 "min": 0, "max": 100, "value": 80},
            ],
            "actions": [
                {"key": "browse", "label": "🔄 浏览分类"},
                {"key": "dosearch", "label": "🔍 搜索"},
                {"key": "volapply", "label": "🔊 应用音量"},
            ],
            "output": {"rows": rows},
        },
    }


def show(rows=None, err=None):
    global UI_SEQ
    UI_SEQ += 1
    all_rows = status_rows()
    if err:
        all_rows.append(err_row(err))
    if rows:
        all_rows += rows
    if not CURRENT and len(all_rows) == 1 and not err:
        all_rows.append(err_row("分类/频道为空：点「🔄 浏览分类」或「🔍 搜索」"))
    send(ui_spec(UI_SEQ, all_rows))


def rows_for_source():
    if LAST_SRC == "search":
        return channel_rows(LAST_CH)
    if CUR_CAT == FAVORITES_ID:
        return channel_rows(load_favs())
    return channel_rows(LAST_CH)


# ---- 播放/停止动作（窗内 ui.event 与面板 activate 两入口共用） -------------

def start_play(cid, title, from_window):
    PLAY_FROM_WIN[0] = from_window
    PENDING_VOL[0] = LAST_VOL[0]
    req("played", 0, "audio.play", {"url": stream_url(cid)})
    global CURRENT
    CURRENT = {"id": cid, "title": title or f"频道 {cid}"}


def stop_play(from_window):
    PLAY_FROM_WIN[0] = from_window
    global CURRENT
    CURRENT = None
    req("stopped", 0, "audio.stop", {})


def panel_stop_row():
    """面板列表顶部的播放状态行（正在播放 → 回车停止）。"""
    if CURRENT:
        return {"title": f"🔊 正在播放：{CURRENT['title']} · 回车停止",
                "subtitle": "宿主侧播放 · 收起面板不中断",
                "payload": "!stop"}
    return None


# ---- 主循环 ----------------------------------------------------------------

def main():
    global CATS, CUR_CAT, CURRENT, LAST_CH, LAST_SRC
    for line in sys.stdin:
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        t = msg.get("type")

        if t == "init":
            send({"type": "ready"})

        elif t == "activate":
            # 面板结果行回车（! 前缀已被宿主剥掉）：回车即播/停止，不弹窗
            payload = msg.get("payload") or ""
            if payload == "stop":
                stop_play(from_window=False)
            elif payload.startswith("play:"):
                cid_s, _, title = payload[5:].partition("|")
                if cid_s.isdigit():
                    start_play(int(cid_s), title, from_window=False)

        elif t == "shutdown":
            return

        elif t == "query":
            text = (msg.get("text") or "").strip()
            low = text.lower()
            # 查询全量广播：只认电台词头
            if low in HINTS:
                if CATS is None:
                    req("cats", 0, "network.fetch", {"url": CATS_URL})
                else:
                    show(rows_for_source())
            else:
                # 电台 <关键词> → 面板列表直出搜台结果（回车即播）；
                # 电台 停止/暂停 → 播放状态行（回车停止）
                head, _, kw = text.partition(" ")
                if head.lower() in HINTS and kw.strip():
                    qid = msg.get("id", 0)
                    kwl = kw.strip().lower()
                    if kwl in ("停止", "stop", "暂停", "pause"):
                        items = []
                        if CURRENT:
                            items.append(panel_stop_row())
                        else:
                            items.append({"title": "⏹ 当前没有在播放",
                                          "payload": "!none"})
                        send({"type": "results", "query_id": qid, "items": items})
                    else:
                        PANEL_QID[0] = qid
                        PANEL_KW[0] = kw.strip()
                        req("panel_search", qid, "network.fetch",
                            {"url": search_url(kw.strip())})

        elif t == "capability_response":
            rid = msg.get("id")
            hit = pending.pop(rid, None)
            if not hit:
                continue
            tag, qid = hit
            ok = msg.get("ok")
            data = msg.get("data") if isinstance(msg.get("data"), str) else ""

            if tag == "cats":
                if not ok:
                    show(err=f"分类获取失败：{msg.get('error', '?')}")
                    continue
                CATS = parse_cats(data) or []
                show(rows_for_source())

            elif tag == "channels":
                if not ok:
                    show(err=f"频道获取失败：{msg.get('error', '?')}")
                    continue
                LAST_CH = parse_channels(data)
                LAST_SRC = "cat"
                show(channel_rows(LAST_CH))

            elif tag == "search":
                if not ok:
                    show(err=f"搜索失败：{msg.get('error', '?')}")
                    continue
                LAST_CH = parse_search(data)
                LAST_SRC = "search"
                if not LAST_CH:
                    show(err="没有匹配的直播电台（只收录直播源）")
                else:
                    show(channel_rows(LAST_CH))

            elif tag == "panel_search":
                if qid != PANEL_QID[0]:
                    continue  # 过期击键的迟到响应，丢弃
                items = []
                stop_row = panel_stop_row()
                if stop_row:
                    items.append(stop_row)
                if not ok:
                    items.append({"title": f"搜索失败：{msg.get('error', '?')}",
                                  "payload": "!none"})
                else:
                    chs = parse_search(data)[:20]
                    if not chs:
                        items.append({"title": f"没有匹配「{PANEL_KW[0]}」的直播电台",
                                      "subtitle": "进电台窗口用分类浏览",
                                      "payload": "!none"})
                    for ch in chs:
                        sub = "回车播放"
                        if ch["np"]:
                            sub = f"正在播：{ch['np']} · {sub}"
                        items.append({"title": ch["title"], "subtitle": sub,
                                      "payload": f"!play:{ch['id']}|{ch['title']}"})
                send({"type": "results", "query_id": qid, "items": items})

            elif tag == "played":
                # 播放成功 → 应用音量；窗内来源才刷窗（面板来源只出声不弹窗）
                if ok and CURRENT:
                    req("volumed", qid, "audio.volume", {"pct": PENDING_VOL[0]})
                elif not ok and PLAY_FROM_WIN[0]:
                    show(err=f"播放失败：{msg.get('error', '?')}")

            elif tag in ("stopped", "volumed", "volumed2"):
                if not ok and PLAY_FROM_WIN[0]:
                    show(err=f"{tag} 失败：{msg.get('error', '?')}")
                elif PLAY_FROM_WIN[0]:
                    show(rows_for_source())

        elif t == "ui.event":
            ev = msg.get("event") or ""
            values = msg.get("values") or {}
            vol = max(0, min(100, int(values.get("vol", 80) or 80)))

            if ev == "action:browse":
                global CUR_CAT
                try:
                    CUR_CAT = int(values.get("cat", "0") or 0)
                except ValueError:
                    CUR_CAT = FAVORITES_ID
                if CUR_CAT == FAVORITES_ID:
                    show(channel_rows(load_favs()))
                else:
                    req("channels", 0, "network.fetch", {"url": channels_url(CUR_CAT)})

            elif ev == "action:dosearch":
                kw = (values.get("kw") or "").strip()
                if not kw:
                    show(err="输入搜索关键字")
                else:
                    req("search", 0, "network.fetch", {"url": search_url(kw)})

            elif ev == "action:volapply":
                LAST_VOL[0] = vol
                req("volumed", 0, "audio.volume", {"pct": vol})

            elif ev.startswith("action:play:"):
                payload = ev[len("action:play:"):]
                cid_s, _, title = payload.partition("|")
                if not cid_s.isdigit():
                    show(err="台号非法")
                    continue
                LAST_VOL[0] = vol
                start_play(int(cid_s), title, from_window=True)

            elif ev == "action:stop":
                stop_play(from_window=True)

            elif ev.startswith("action:fav:"):
                payload = ev[len("action:fav:"):]
                cid_s, _, title = payload.partition("|")
                if cid_s.isdigit():
                    favs = load_favs()
                    if not any(f["id"] == int(cid_s) for f in favs):
                        favs.append({"id": int(cid_s), "title": title or f"频道 {cid_s}"})
                        save_favs(favs)
                    show(rows_for_source())

            elif ev.startswith("action:unfav:"):
                cid_s = ev[len("action:unfav:"):]
                if cid_s.isdigit():
                    save_favs([f for f in load_favs() if f["id"] != int(cid_s)])
                    show(rows_for_source())


if __name__ == "__main__":
    main()
