# Android 侧：NsdServiceManager 能否发 RAVENNA 的 DNS-SD 子类型？

**结论（2026-11-24 核实）：Android 11（Z9X 8K，API 30）不能。** 子类型 API 直到
**API 35（Android 15）** 才进入公开 SDK。

## 证据

- NsdServiceInfo.getSubtypes() / setSubtypes(Set<String>) 在 API diff 35 中标记为
  "Added Methods"（From Level 34 -> To Level 35）：
  https://android-dotdevsite-v2-prod.appspot.com/sdk/api_diff/35/changes/android.net.nsd.NsdServiceInfo
- Android 官方参考（getSubtypes 说明）：返回值在浏览时可能不含子类型；
  https://developer.android.com/reference/android/net/nsd/NsdServiceInfo
  文档对 subtypes 元素的格式要求：去掉尾部 "._sub"，例如 _printer 对应
  _printer._sub._http._tcp.local。
- Android < 15 想注册子类型的唯一已知做法是“复合服务类型串”这一**未公开行为**
  （project-chip/connectedhomeip PR #15177 的注释原话：subtypes registration is
  using an undocumented feature of android dns-sd service/mDNSResponder which MAY
  STOP WORKING in future Android versions）。本项目未在该固件上验证。
  https://github.com/project-chip/connectedhomeip/pull/15177/files

## 这对 RAVENNA 意味着什么

RAVENNA 的发现靠子类型（Operating Principles 3.5.1 / 3.5.2.1）：

    _rtsp._tcp        + _ravenna._sub._rtsp._tcp            （节点 RTSP 服务）
    _http._tcp        + _ravenna._sub._http._tcp            （节点 HTTP 服务）
    _rtsp._tcp        + _ravenna_session._sub._rtsp._tcp    （每个会话）

在 API < 35 上只能公告基础类型。按 RFC 6763 §7.1，子类型是**额外**的 PTR，
普通类型注册不会自动产生子类型 PTR，所以：

- 只 browse _rtsp._tcp 的通用浏览器**能**看到我们（aes67 的探测脚本就是这么做的），
- 但 browse _ravenna_session._sub._rtsp._tcp 的 RAVENNA 控制器**看不到**我们。

## 可选路线（按推荐度）

1. **Android 15+ 设备 / 升级固件**：用 setSubtypes，代码已按版本分支写好
   （见 NsdAdvertiser.kt，Build.VERSION_CODES.VANILLA_ICE_CREAM 分支）。
2. **原生进程共享 5353（Rust mDNS 应答器 + SO_REUSEADDR）**：能发完整子类型，
   但要与系统 mDNS 守护共存：应答重复、缓存冲突，风险高，需真机实测
   （设备上 5353 已被 UID 1000 占用，见报告 §3.3）。
3. **PC/网关侧代为公告**：设备只提供 RTSP，发现层由 PC 上跑的原型（本 crate）或
   控制台 App 所在 PC 代发。最稳、零设备风险，但不是“设备自己能被发现”。
4. **复合类型串 hack**：跑起来最快，但依赖未公开行为，跨固件/版本随时失效。
   存在两种未文档化的写法，均需真机验证：`_subtype._sub._type._tcp`（NsdServiceInfo.serviceType
   直接塞完整子类型串，mDNSResponder 内部解析）与 `_type._tcp,_subtype`（逗号形式，Android 14+
   Connectivity 模块的 MdnsSearchOptions 解析器里有对应分支）。已在 NsdAdvertiser.kt 做成
   USE_COMPOSITE_HACK 开关，默认关闭。

## 文件

- NsdAdvertiser.kt：可直接贴进 Compose App 的公告类（节点 RTSP/HTTP + 会话）。
  只做公告，不做浏览；未编译（Rust 侧才是本任务主体，Kotlin 仅作接口交付）。
- 端口约定：RTSP 8554（Rust daemon），HTTP 8080（未来配置页）。
- TXT：默认只发 node-id；设备名/版本/采样率/声道数可选，规范允许但不必要。

## 未验证项

- 未在 Z9X 上编译/运行；未验证 NsdManager 在 Android 11 上对 "有空格实例名"
  的处理（RAVENNA 的 vendor node id 允许空格）。
- 未验证系统 mDNS 守护与自建 5353 进程共存的实际行为（路线 2）。
