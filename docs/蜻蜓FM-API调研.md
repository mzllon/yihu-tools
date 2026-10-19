# 蜻蜓 FM API 调研存档（电台插件数据源契约）

> 调研日期：2026-10-16 / 2026-10-19，全部端点经 curl 实测验证。
> 用途：radio native 插件（docs/电台native插件设计.md §4.5）的数据契约；
> 端点常量实现时集中一处（radio-api.rs），便于上游变更时热修。
> 情报来源：本仓库实测 + 墨鱼 FM 扩展（闭源，Edge 商店 ljekbbjjbpnkpnlfmcleklciinjbiapl）
> 安装包逆向提取的端点清单交叉印证。

## 0. 主机与总则

| 主机 | 用途 |
|---|---|
| `rapi.qtfm.cn` | 主 API：分类 / 频道 / 地区 / 榜单 |
| `rapi.qingting.fm` | 频道详情（v4）/ 批量正在播（v2，**本插件不采用**） |
| `search.qingting.fm` | 搜索 |
| `ls.qingting.fm` | HLS 直播流 |
| `pic.qtfm.cn` | 封面图（**http:// 非 https**，客户端两 scheme 都要支持） |

- 全部端点**无鉴权**、无 SLA；响应包裹层不统一（见各端点标注），解析勿假设。
- 频道条目公共字段：`content_id, title, cover(图URL), description,
  nowplaying{id,name,start_time,end_time,broadcasters}, audience_count,
  update_time, categories[{id,title,pid}], region{id,title,pid},
  city{id,title,pid}, clout(热度)`。

## 1. 分类列表

`GET rapi.qtfm.cn/categories?type=channel`
→ `{Success:"ok", Data:[{id,title}]}`

实测 13 类：433 资讯台 / 442 音乐台 / 429 交通台 / 439 经济台 / 432 文艺台 /
441 都市台 / 430 体育台 / 431 双语台 / 440 综合台 / 438 生活台 / 435 旅游台 /
436 曲艺台 / 434 方言台。
另有 `type=region` → `[{id:-1,title:"省市台"}, {id:407,title:"网络台"}]`（特殊类目）。

## 2. 分类下频道（分页）

`GET rapi.qtfm.cn/categories/{cat}/channels?page=1&pagesize=50`
→ `{Success, Data:[频道条目...]}`（Data 为平铺 list，无 total 字段）

- 分页实测可用（page=2 返回不重复内容）；无 total，翻页终止靠空页判定。
- `region_id` 参数**被服务端忽略**（region 3 与 31 返回相同混合列表，实测）。

## 3. 地区

`GET rapi.qtfm.cn/regions?all=true` → `{Success, Data:[{id,title}]}`
实测 32 条：407 网络台 + 31 省级行政区（3 北京 / 5 天津 / 7 河北 / 83 上海 /
99 浙江 / 217 广东 / 259 四川 …全量见请求）。

- `?type=channel` 等价（不含网络台时用这个）。
- 频道条目的 region/city 自带 pid 层级（city.pid = region.id）→ 城市列表可从
  已拉取频道归组，无需专门端点。
- **按地区列台没有直接端点**：`/regions/{id}/channels`、`/cities/{id}/channels`、
  `region_id=` 参数等 8 种形态实测全部 404 或被无视。**正解 = billboards 组合
  榜单（见 §4）**，或客户端对任意列表按 region.id 过滤。

## 4. 榜单：分类 × 地区组合（地区内容正解）

`GET rapi.qtfm.cn/billboards/{分类id}/{地区id}/channels`
→ `{Success, Data:[频道条目...]}`

- 分类 id 用 `0` = 全部分类。实测：`/billboards/0/99/channels`（全类×浙江）
  20 条；`/billboards/429/99/channels`（交通台×浙江）13 条。
- 地区 id 取自 §3；「网络台」407 亦可作地区维度尝试。
- 排序语义为热度榜（墨鱼 FM 的「分类排行 / 地区排行」即此端点）。

## 5. 搜索

`GET search.qingting.fm/v3/search?k={urlencoded关键词}`
→ `{errcode, errmsg, data:{data:{docs:[...]}}}`（包裹层与前两者不同，注意）

- docs 条目字段：`id, id_str, title, type, podcaster, cover, updatetime,
  description, playcount, program_count, url_schema, ...`
- **type 过滤**：`channel_live` = 直播台（本插件只取这类）；`channel_ondemand`
  = 点播/播客（也会被搜出，需丢弃或留待后续点播功能）。
- 搜「杭州交通」这类城市+类型词可能全命中点播（实测 30 条 0 直播）——搜索
  建议按台名/频率号。

## 6. 播放流

`https://ls.qingting.fm/live/{content_id}/64k.m3u8`
实测 HTTP 200（HLS playlist）。由频道 id 直接拼 URL，无需请求。

## 7. 未采用（记录备查）

`GET rapi.qingting.fm/v2/livechannelplaying?ids={逗号分隔}&current_time={epoch}`
——批量正在播（多频道一次查）。用户决策（2026-10-19）不采用；单频道正在播
用 §2 列表自带 nowplaying 或 §8 详情。
另 `GET rapi.qingting.fm/v4/channels/{id}` = 频道详情（墨鱼 FM 在用；字段结构
待实现时实测校验一次，确认正在播与码率信息）。

## 8. 防御注意（实现硬性要求）

1. 全请求带超时；失败降级到收藏本地可用（API 无 SLA）。
2. 端点常量集中一处；上游有变更史，热修不动逻辑。
3. 包裹层三种形态（`{Success,Data}` / `{errcode,data}` / v4 未知）——解析层
   分别对待，勿写通用假设。
4. 封面 http:// 与 https:// 混用；图片加载失败时用频道名首字渐变兜底。
5. 域名清单：rapi.qtfm.cn / rapi.qingting.fm / search.qingting.fm /
   ls.qingting.fm / pic.qtfm.cn（native 插件直连白名单，如需审计对账）。
