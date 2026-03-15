# AGENTS.md

本文件是本仓库的最高优先级工程约束与持续记忆文件。进入任何目录、阅读/修改任何代码前，必须先阅读本文件。之后每完成一个有意义的步骤，必须回写本文件，确保要求、决策、进度、风险与下一步始终是最新状态。

---

## 1. 项目摘要

目标是构建一个 **Windows 10/11 本地 Rust 桌面程序**，用于在 **嘈杂、人物密集、近距离串音** 的直播环境中，从 **单麦克风输入** 里尽可能只保留 **用户本人声音**，抑制附近其他人的声音，然后把处理结果输出到 **虚拟麦克风链路**，供 **OBS / Discord** 等软件当作普通麦克风使用。

程序必须同时包含：

1. **训练 / 注册环节**
   - 程序引导用户录制固定短句与自由发挥语音；
   - 基于预训练声纹/说话人模型生成用户 profile；
   - 得到后续推理使用的目标说话人过滤依据。

2. **实时推理环节**
   - 监听真实麦克风；
   - 基于用户 profile 做目标说话人过滤；
   - 去除或尽可能压制非用户本人的声音；
   - 把输出送到虚拟音频设备链路。

---

## 2. 硬约束（必须遵守）

### 2.1 平台与语言
- 操作系统目标：**Windows 10 / Windows 11**
- 编程语言：**Rust**
- Rust toolchain：**nightly-2025-07-12**
- GUI：**eframe / egui**

### 2.2 运行方式
- 必须是 **本地运行** 的桌面程序。
- **不得依赖云端推理**。
- v1 必须优先保证 **可运行、可调试、可接入 OBS/Discord**。

### 2.3 虚拟麦克风策略
- **v1 不自研 Windows 虚拟麦克风驱动**。
- v1 采用 **现成虚拟音频线设备** 的方式接入：
  - 程序输出到虚拟音频线的 **播放端**；
  - OBS / Discord 选择其对应的 **录音端** 作为麦克风输入。
- 只有当用户后续明确提出、且 v1 已经稳定，才允许单开驱动工程进行探索。

### 2.4 “训练”定义
- 本项目中的“训练”默认定义为：
  - **enrollment / registration / calibration**
  - 即：基于预训练模型进行用户声纹注册、阈值校准、profile 生成
- **不是** 从零开始训练大型深度模型。
- 如需离线准备 ONNX 模型转换脚本，可放在 `tools/model_prep/`，但不得成为最终用户运行时依赖。

### 2.5 实时性
- 音频回调线程中：
  - **禁止阻塞锁**
  - **禁止磁盘 I/O**
  - **禁止网络 I/O**
  - **禁止大规模分配**
  - **禁止重型推理**
- 所有重型处理必须放到独立 worker / pipeline 线程。

---

## 3. 产品定义与边界

### 3.1 要解决的问题
用户在直播时，与旁边其他人距离不到 1m，真实麦克风会明显收进其他人的声音。需要一个程序，在单麦条件下尽量只保留用户本人声音，并将结果作为虚拟麦克风输出。

### 3.2 真实边界
- 这是 **目标说话人提取 / 目标说话人过滤** 问题，不只是普通降噪。
- 在 **单麦克风、多人近距离、同时说话** 条件下，无法承诺 100% 完全清除他人声音。
- 产品目标应定义为：
  - **尽可能保留用户声音**
  - **尽可能压制非用户声音**
  - **在可接受延迟下保持语音可懂度与稳定性**

### 3.3 v1 非目标
- 不追求完美声学分离。
- 不自研深度学习训练框架。
- 不开发自定义内核级虚拟音频驱动。
- 不做跨平台（Linux/macOS 暂不支持）。
- 不做云端账号、同步、联网服务。

---

## 4. 技术路线总原则

### 4.1 总体路线
采用 **Rust 用户态应用 + ONNX Runtime + 现成虚拟音频线** 的工程路线。

### 4.2 核心思想
将系统拆成两层：

1. **基础可运行链路**
   - 真实麦克风采集
   - GUI 控制
   - 稳定输出到虚拟音频线
   - 先打通 OBS / Discord 接入

2. **目标说话人过滤链路**
   - VAD（语音活动检测）
   - Speaker Embedding（说话人特征提取）
   - Speech Enhancement（基础语音增强）
   - TSE / speaker-conditioned filtering（目标说话人提取）

### 4.3 模式设计
程序至少应支持三种模式：

1. `Passthrough`
   - 仅做最基础通路；
   - 用于设备调试与延迟定位。

2. `Basic Filter`
   - VAD + speaker gate + enhancement；
   - 作为稳定的首个可用模式。

3. `Strong Isolation`
   - 在 `Basic Filter` 基础上加入真正的 TSE；
   - 标记为实验性，直到效果稳定。

---

## 5. 建议依赖与选型

以下选型为当前默认方案；除非遇到明确不可行问题，否则不要随意替换。

### 5.1 Rust 侧依赖
- GUI：`eframe`, `egui`
- 音频 I/O：`cpal`
- ONNX Runtime：`ort`
- 重采样：`rubato`
- ring buffer：`rtrb`
- WAV 读写：`hound`
- 序列化：`serde`, `serde_json`
- 错误处理：`anyhow`, `thiserror`
- 日志：`tracing`, `tracing-subscriber`
- 文件路径：`camino`（可选）
- 配置目录：`directories` 或 `directories-next`（可选）

### 5.2 模型路线
默认使用 ONNX 统一部署，首选模型类别如下：

- **VAD**：Silero VAD ONNX
- **Speaker Embedding**：WeSpeaker ONNX（优先） / ECAPA-TDNN（备选）
- **Enhancement**：GTCRN ONNX
- **TSE / 目标说话人提取**：WeSep 导出的 ONNX（如集成可行）

注意：
- 所有模型都必须通过统一抽象封装，不能把具体模型细节散落到 UI 或音频模块。
- 模型路径、采样率、输入输出 shape、版本、hash、license 信息必须记录在 `models/manifest.json`。

### 5.3 推理后端策略
- 默认 CPU 先跑通。
- GPU 加速属于后续增强项。
- Windows 上如后续需要，可探索 ONNX Runtime + DirectML，但不是 M0~M3 的前置要求。

---

## 6. 音频架构

### 6.1 总体链路
`真实麦克风输入 -> 输入回调 -> 输入 RingBuffer -> DSP/ML Worker -> 输出 RingBuffer -> 输出回调 -> 虚拟音频线播放端 -> OBS/Discord 选择其录音端`

### 6.2 输入回调职责
输入回调只能做轻量工作：
- 读取音频帧
- 转换为统一内部格式（推荐 `f32`）
- 写入 ring buffer
- 记录必要但轻量的统计信息

禁止在输入回调中：
- 模型推理
- 磁盘写入
- 阻塞等待
- GUI 更新
- 大块内存分配

### 6.3 DSP/ML Worker 职责
Worker 线程负责：
- 重采样到模型采样率
- 单声道处理
- 分帧与重叠
- VAD
- speaker embedding / similarity
- enhancement
- TSE（如启用）
- 平滑/增益/限幅
- 结果写入输出 ring buffer

### 6.4 输出回调职责
输出回调只能做轻量工作：
- 从输出 ring buffer 取处理后的帧
- 不足时补零或按既定策略填充
- 写到指定输出设备（虚拟音频线播放端）

### 6.5 内部音频格式建议
- 内部处理优先统一为：`mono`, `f32`
- 首选内部推理采样率：`16_000 Hz`
- 设备侧可根据输入/输出设备规格做必要重采样
- 所有采样率变化必须经过明确模块，不允许“隐式假设”

---

## 7. 训练 / 注册（Enrollment）设计

### 7.1 训练流程定义
训练并不意味着从零训练神经网络，而是执行用户声纹注册流程：

1. 选择输入设备
2. 录制短暂环境静音
3. 录制固定短句
4. 录制自由发挥语音
5. 执行 VAD 切片与质量检查
6. 提取 embedding
7. 聚合为用户 profile
8. 估计阈值/置信区间
9. 保存 profile 与必要元数据

### 7.2 固定短句要求
- 中文为主
- 数量固定为：**10 条**
- 覆盖：
  - 常见元音
  - 塞音/擦音/鼻音
  - 数字
  - 直播常用口播词
  - 快速语速与正常语速
- 固定短句文本保存在：
  - `assets/prompts_zh_cn.txt`

### 7.3 自由发挥要求
- 固定时长：**30 秒**
- 内容可以是自我介绍、直播常说内容、自然口语
- 目标是覆盖：
  - 自然连读
  - 停顿
  - 情绪变化
  - 不同语速

### 7.4 质量检查
训练数据需进行最低限度质量检查：
- 有效语音总时长
- 平均音量
- 音量过低/过高
- 削波风险
- 背景噪声偏高
- VAD 后有效片段数量是否足够
- 如果样本明显不合格，应提示重新录制

### 7.5 Profile 内容
每个用户 profile 至少包含：
- 用户标识
- 创建时间
- 模型版本信息
- embedding 列表摘要
- centroid / mean embedding
- variance / dispersion 指标
- 建议阈值
- 训练参数
- 原始音频或清洗音频路径（可选，但推荐保留）

Profile 建议保存为：
- `profiles/default/speaker_profile.json`

---

## 8. 推理设计

### 8.1 推理模式
必须至少实现：
- `Passthrough`
- `Basic Filter`

`Strong Isolation` 可作为后续里程碑，但架构必须预留。

### 8.2 Basic Filter 预期逻辑
建议基本链路：
1. 设备输入
2. 重采样 / 单声道
3. VAD
4. 若是非语音，进行噪声底处理或直接低输出
5. 若是语音，提取当前分帧 embedding
6. 与用户 profile 做 similarity 估计
7. 根据 similarity 做 gate / gain 调整
8. 对保留段做基础 enhancement
9. 输出到虚拟音频线

### 8.3 Strong Isolation 预期逻辑
在 `Basic Filter` 之上增加：
- 以用户 profile/参考 embedding 作为条件输入
- 执行 TSE，尽可能只保留目标说话人
- 需要在 UI 中明确标为实验性，直到稳定

### 8.4 实时保护
必须加入以下机制：
- ring buffer underrun / overrun 监控
- 模型推理失败时自动回退
- 输出静音保护
- 峰值限制/限幅，防止爆音
- 设备断开后不应直接崩溃

---

## 9. UI 设计要求

### 9.1 页面结构
最少包含以下页面/区域：

1. **设备页**
   - 输入设备选择
   - 输出设备选择
   - 采样率/通道基本信息
   - 启动/停止实时处理

2. **训练页**
   - 引导式录音流程
   - 固定短句显示
   - 自由发挥录制
   - 质量检查结果
   - profile 保存与加载

3. **推理页**
   - 模式选择：Passthrough / Basic Filter / Strong Isolation
   - 默认 profile 状态展示
   - 实时状态指示
   - 简单电平/相似度/工作状态展示

4. **调试页**
   - 日志视图
   - 缓冲区状态
   - 当前模型信息
   - 导出诊断数据

### 9.2 UI 原则
- 先可用，再美化
- 优先清晰状态展示
- 错误必须可见、可理解
- 设备不可用时要有明确提示
- 实验性功能必须明确标注，不得伪装成稳定功能
- 训练页必须采用强引导流程，用户每点击“确定准备好”或“确定完成”后才允许进入下一步，不允许随意跳步

---

## 10. 仓库结构约定

默认采用如下结构；如确需调整，必须先更新本文件再改：

```text
/
├─ AGENTS.md
├─ Cargo.toml
├─ rust-toolchain.toml
├─ assets/
│  └─ prompts_zh_cn.txt
├─ models/
│  ├─ manifest.json
│  └─ ...
├─ profiles/
│  └─ ...
├─ tools/
│  └─ model_prep/
├─ src/
│  ├─ main.rs
│  ├─ app/
│  │  ├─ mod.rs
│  │  ├─ state.rs
│  │  └─ commands.rs
│  ├─ ui/
│  │  ├─ mod.rs
│  │  ├─ devices.rs
│  │  ├─ training.rs
│  │  ├─ inference.rs
│  │  └─ debug.rs
│  ├─ audio/
│  │  ├─ mod.rs
│  │  ├─ devices.rs
│  │  ├─ capture.rs
│  │  ├─ render.rs
│  │  ├─ buffers.rs
│  │  └─ resample.rs
│  ├─ ml/
│  │  ├─ mod.rs
│  │  ├─ runtime.rs
│  │  ├─ vad.rs
│  │  ├─ speaker.rs
│  │  ├─ enhancement.rs
│  │  └─ tse.rs
│  ├─ pipeline/
│  │  ├─ mod.rs
│  │  ├─ realtime.rs
│  │  └─ frames.rs
│  ├─ profile/
│  │  ├─ mod.rs
│  │  ├─ record.rs
│  │  ├─ quality.rs
│  │  ├─ build.rs
│  │  └─ storage.rs
│  ├─ config/
│  │  ├─ mod.rs
│  │  └─ settings.rs
│  └─ util/
│     ├─ mod.rs
│     ├─ audio_math.rs
│     └─ time.rs
└─ tests/
   └─ ...
```

## 11. 开发顺序（不得跳步）

### 11.1 M0：工程初始化

**目标**：建立最小可编译、可运行 GUI 壳子与工程骨架。

**必须完成**：
- `rust-toolchain.toml` 固定到 `nightly-2025-07-12`
- `eframe/egui` 窗口跑起来
- `tracing` 日志初始化
- 基础配置读写
- 目录结构落地
- `AGENTS.md` 首次填充并进入持续更新状态

**完成标准**：
- `cargo build` 成功
- 程序窗口可打开
- 基础日志可输出

### 11.2 M1：音频设备与直通

**目标**：打通 “真实麦克风 -> 程序 -> 虚拟音频线播放端”。

**必须完成**：
- 枚举输入/输出设备
- 选择真实麦克风输入
- 选择虚拟音频线播放端输出
- 实现 `Passthrough`
- 基本电平显示

**完成标准**：
- 程序可把输入声音送到虚拟音频线
- OBS / Discord 可从对应录音端收到声音

### 11.3 M2：训练/注册流程

**目标**：完成 profile 生成与保存。

**必须完成**：
- 固定短句录制
- 自由发挥录制
- 质量检查
- embedding 提取
- profile 保存/加载
- UI 展示默认 profile 状态

**完成标准**：
- 能成功生成 `speaker_profile.json`
- 能重新加载 profile

### 11.4 M3：离线推理验证

**目标**：在实时前先把离线算法跑通。

**必须完成**：
- 使用 WAV 文件做离线测试
- `Basic Filter` 对离线输入有效
- 输出 WAV 文件可听检验
- 记录关键指标与主观结果

**完成标准**：
- 能稳定处理离线样本
- 不依赖实时回调即可验证算法有效性

### 11.5 M4：实时 Basic Filter

**目标**：完成首个真实可用的过滤模式。

**必须完成**：
- VAD 接入实时链路
- speaker similarity gating
- enhancement 接入
- GUI 中可启停 `Basic Filter`
- 日志与状态可视化

**完成标准**：
- `Basic Filter` 在实时输入中明显优于 `Passthrough`
- 程序不因短时间设备波动直接崩溃

### 11.6 M5：Strong Isolation

**目标**：接入真正的 TSE。

**必须完成**：
- TSE 模块抽象
- 条件输入使用用户 profile/embedding
- 实时链路接通
- UI 标注为实验性功能

**完成标准**：
- `Strong Isolation` 可运行
- 比 `Basic Filter` 在部分场景更强
- 若效果不稳定，必须诚实标注

### 11.7 M6：工程化与稳定性

**目标**：提高可交付质量。

**必须完成**：
- 设备切换/断开恢复策略
- 错误处理完善
- 调试导出功能
- 配置持久化
- 文档与打包说明

**完成标准**：
- 日常使用不会因常见异常直接崩溃
- 能指导用户完成基本配置与使用

## 12. 编码规则

### 12.1 通用规则
- 优先写清晰、可维护代码。
- 避免“先写死、以后再说”的核心路径设计。
- 禁止把模型、设备、UI、状态管理耦合在一个文件中。
- 公共抽象先于重复实现。
- 不得引入没有必要的大型框架。

### 12.2 错误处理
- 用户可见错误必须提供可理解信息。
- 内部错误必须带上下文。
- 不得大量 `unwrap()` / `expect()` 在生产路径中使用。
- 可恢复错误优先恢复，不可恢复错误要明确失败原因。

### 12.3 实时音频规则
- 音频线程不能阻塞。
- 热路径避免动态分配。
- 共享状态尽量用 lock-free / 最小锁方案。
- 大对象预分配，尽量复用缓冲区。

### 12.4 模块边界
- `ui/` 不直接操作底层 ONNX session。
- `ml/` 不直接依赖 GUI 组件。
- `audio/` 不持有复杂业务逻辑。
- `pipeline/` 负责把音频与模型链接起来。

### 12.5 测试策略
优先补充：
- profile 序列化/反序列化测试
- 音频格式转换测试
- 重采样正确性测试
- 离线推理回归测试
- 关键状态机测试

## 13. 质量与验收标准

必须满足以下最低验收标准：

- 程序能在 Windows 10/11 上启动。
- 能枚举并选择真实麦克风输入设备。
- 能枚举并选择虚拟音频线播放端输出设备。
- OBS / Discord 能使用对应录音端作为麦克风。
- 能完成一次训练/注册并成功保存 profile。
- `Passthrough` 可正常工作。
- `Basic Filter` 可正常工作。
- 若 `Strong Isolation` 未完成，UI 必须明确标注未完成或实验性。
- 设备断开、模型加载失败、profile 缺失等情况下，程序不能直接崩溃。
- `AGENTS.md` 在开发过程中保持最新。

## 14. 风险与现实说明

以下结论必须贯穿设计与文档：

- 单麦、近距离、多人同时说话场景非常困难。
- 本项目目标是“尽可能压制非目标说话人”，不是保证绝对纯净。
- v1 的首要目标是可运行、可调试、可接入直播链路。
- 先交付可用的 `Basic Filter`，再推进 `Strong Isolation`。
- 若某模型效果不稳定，优先保留模块化接口，不要硬编码死锁在某单一模型上。

## 15. 开发行为准则（对 Codex 的强约束）

每次开始工作时，必须按以下顺序执行：

1. 阅读本文件。
2. 检查“当前确认需求”“当前决策”“当前里程碑状态”。
3. 如用户提出了新要求：
   - 先更新本文件；
   - 再开始改代码。

每次只推进一个清晰的小步骤，不要同时大范围改多个方向。

每完成一个有意义步骤后：
- 更新本文件的进度与记忆；
- 再继续下一步。

### 15.1 严禁行为
- 严禁跳过 M0/M1 直接做复杂模型集成。
- 严禁在没有直通链路前先做花哨 UI。
- 严禁在没有 profile 保存/加载前把训练流程写死。
- 严禁无说明地替换关键选型。
- 严禁改完代码不更新本文件。

### 15.2 必须更新 AGENTS.md 的场景
以下任一情况发生时，必须更新本文件：

- 用户新增要求
- 用户修改约束
- 关键选型发生变化
- 完成一个里程碑子任务
- 发现重要风险或阻塞
- 决定放弃某方案并切换
- 新增重要文件结构或模块边界
- 新增模型、设备、格式兼容要求

## 16. 当前确认需求（持续更新）

本节必须始终保持为最新摘要。

- 项目是本地 Windows 10/11 Rust 桌面程序。
- Rust toolchain 固定为 `nightly-2025-07-12`。
- GUI 必须使用 `eframe/egui`。
- 程序监听真实麦克风输入。
- 程序输出到虚拟麦克风链路，供 OBS/Discord 使用。
- 目标是在嘈杂、人物密集、近距离串音环境中尽可能保留用户本人声音。
- 训练环节包含固定短句与自由发挥两部分。
- 训练结果用于生成目标说话人过滤依据。
- 程序必须包含训练环节与实时推理环节。
- 开发过程中，新的要求/重要记忆/需求/已完成内容必须持续更新到 `AGENTS.md`。
- 当前执行要求：严格按里程碑顺序从 M0 开始，不跳步；每完成一个有意义步骤都必须先更新 `AGENTS.md`。
- 新增 UI 要求：程序需内置思源黑体，并统一使用思源黑体 Regular 渲染所有界面字体，避免中文显示为方块。
- 用户已确认当前状态：输入设备可采集麦克风，输出设备可用于本地监听，但 OBS 仍只能选择原始输入麦克风，不能选择程序提供的虚拟麦克风。
- 用户新增系统级要求：程序启动时就应向 Windows 提供一个可被 OBS / Discord 选择的虚拟麦克风设备，而不只是把音频输出到普通播放设备。
- 用户已确认最终路线：保持原 v1 方案，不自研虚拟麦克风驱动，继续依赖现成虚拟音频线/虚拟麦克风驱动，由程序自动识别和接管。
- 用户已现场确认：`VB-CABLE` 已安装，当前 `Passthrough` 输出到播放设备功能正常。
- 用户新增产品约束：程序为单人单机固定使用，不需要角色切换或多 profile 选择；v1 改为单一默认 profile 模式。
- 用户新增训练要求：训练页必须采用强引导逐步确认流程；固定短句缩短为 10 句；自由发挥固定为 30 秒。
- 用户新增训练页细化要求：环境静音步骤中的秒数需以倒计时形式显示；固定短句 `01/10` 开始前必须先有用户准备环节；自由发挥开始前也必须先有用户准备环节。
- 用户新增训练页交互要求：主确认按钮下方需提供“有一段没录好，重新录制上一句话”按钮，用于回退到上一条固定短句重录。
- 用户新增训练页行为要求：环境静音倒计时结束后需自动进入固定短句准备，不再要求用户手动确认；“当前输入设备”下方需加粗显示麦克风录制状态，未录制为红色“当前麦克风未录制”，录制中为绿色“当前麦克风录制中”。
- 用户新增固定短句流程要求：每一句固定短句前都必须有单独准备阶段，准备阶段需要展示该句文本；固定短句录制页主按钮文案改为“确认本句已完成，进入下一个准备阶段”。
- 用户新增字体要求：训练页中的“当前麦克风未录制 / 当前麦克风录制中”必须使用真正可见的加粗字重，不能只依赖 `strong()` 对 Regular 字体做样式强调；可内置思源黑体 Bold 专门用于该状态文本。
- 用户新增状态图标要求：在“当前麦克风未录制 / 当前麦克风录制中”左侧增加暂停/播放 icon；未录制使用暂停，录制中使用播放；两个 icon 必须使用一致的固定宽度，不能因图标本身导致文本对齐错位。
- 用户新增对齐要求：播放/暂停 icon 必须与“当前麦克风未录制 / 当前麦克风录制中”这句文字的水平中线对齐，不能出现 icon 明显高于文本的情况。
- 用户新增修正要求：只允许修正 icon 与状态文本的中线对齐，不能因为对齐修正把整句状态文本移到页面中间，也不能影响其它元素布局。
- 用户新增完成确认要求：训练页“第 25 步：完成确认”中的“重新开始本轮训练”必须连续点击 3 次，才允许丢弃本轮训练信息并重新开始。
- 用户新增 M2 实现要求：训练流程需要接入真实录音会话，实际采集环境静音、10 句固定短句和 30 秒自由发挥音频。
- 用户新增训练页反馈要求：在“当前麦克风未录制 / 当前麦克风录制中”右侧增加条形音量指示器，用于让用户感知麦克风正在被程序占用和采集。
- 当前训练录音的工程约束：开始训练前必须先停止设备页中的 `Passthrough`，避免同一麦克风被实时链路与训练录音同时占用。
- 用户新增训练页摘要要求：`固定短句：已录制 x/10 句` 需改为可展开按钮，展开后展示全部固定短句文本与各句录制状态。
- 用户新增窗口交互要求：主内容区需要提供可上下滚动的纵向滚动条，长页面不能被窗口高度截断。
- 用户新增窗口布局修复要求：滚动到页面底部时，底部状态栏不能遮挡主内容区最后几行内容；滚轮必须能滚到完整底部。
- 用户新增训练页防误触要求：“有一段没录好，重新录制上一句话”按钮也必须连续点击 3 次后才允许真正回退生效。
- 用户新增仓库管理要求：将目前为止已完成的全部内容按功能块拆成几个 Git commit，并写清楚 commit message。
- 用户新增 Review 细化要求：“展开查看有问题的录音片段”中的环境静音与自由发挥也必须提供“重录”“预览”入口，不能只覆盖固定短句。
- 用户新增 Review 一致性要求：重录环境静音时也必须使用 5 秒倒计时，再进入重录录制，保持与主训练流程语义一致。
- 用户新增基础质检要求：不要向用户提示爆音风险或平均音量偏高；当判定“有效语音不足”时，不再叠加“平均音量偏低”提示；即使基础检查存在错误，也仍然允许用户继续下一步操作。
- 用户新增 Review 阶段录音管理要求：在“当前录音结果”里的环境静音 / 每句固定短句 / 自由发挥前增加“重录”“预览”按钮，其中这里的“重录”必须连续点击 3 次才真正触发。
- 用户新增问题片段交互要求：在“展开查看有问题的录音片段”中，每个有问题的固定短句前增加“重录”“预览”按钮；这里的“重录”只需点击 1 次触发。
- 用户新增定向重录流程要求：从 Review 阶段进入固定短句重录时，上方强引导框需显示“第 1 步：重录固定短句准备 xx”“第 2 步：重录固定短句 xx”；重录完成后页面必须自动返回“第 25 步：完成确认”并恢复该阶段全部信息。
- 用户新增启动期录音检测要求：程序启动时若检测到 `profiles/default/recordings` 下存在训练录音，需检查录音文件是否齐全，以及是否与 `profiles/default/speaker_profile.json` 一一对应。
- 用户新增启动期提示要求：若录音完整且与默认 profile 对应，则提示用户检测到之前保存的录音；若录音缺失或存在其他不应存在的杂项文件，则显示警告说明目录不完整或有异常文件。
- 用户新增启动期操作要求：检测到旧录音后，用户可选择“全部覆盖重录”或“加载文件”；选择“加载文件”后，应等价于直接跳至“第 25 步：完成确认”，并按刚完成录制那样触发一次质量检查。
- 用户新增启动期防误触要求：启动期弹窗中的“全部覆盖重录”也必须连续点击 3 次后才允许真正触发。
- 用户当前执行要求：把当前未提交的 M2/M3 相关工作按功能块整理成几个 Git commit，并为每个 commit 写清楚 `commit -m` 信息。
- 用户当前执行要求：将整理后的全部 commit 统一推送到 `git@github.com:SEmmmer/EKSingleMic.git`。
- 用户已于 2026-03-16 确认第五轮离线 `refscore` 试听结果“已经可以了”，同意结束 M3 并进入下一步 M4 实时 `Basic Filter`。
- 用户已于 2026-03-16 完成首轮 M4 真机听检：实时 `Basic Filter` 可稳定启动，无明显延迟，目标说话人能保持住，旁人声音相较 `Passthrough` 有一定压制效果；但“加载旧录音”和“启动 `Basic Filter`”时界面会短暂卡顿，用户新增要求是在这两处提供清晰的进度条/加载提示，降低等待焦虑。
- 用户已于 2026-03-16 对上一轮等待反馈实现提出修正要求：不接受统一忙碌弹窗；“检测到之前保存的录音”区域必须直接显示进度条；“开始推理/启动实时链路”区域必须改为局部转圈按钮；在这两处交互都达到可用前，不应交还给用户。
- 用户已于 2026-03-16 进一步确认：当前局部提示仍不够，必须把“加载旧录音”和“启动实时链路”两条路径改成真正的异步后台任务，并提供分阶段进度，而不是只在下一帧延后执行同步重活。
- 用户已于 2026-03-16 追加真机反馈：点击“启动 Basic Filter”时仍未实际看到局部转圈按钮；当前问题不是没有异步任务，而是启动态在 UI 上没有稳定可见地呈现出来，需保证点击后立即重绘并让局部 loading 至少可见一个短暂但明确的时长。
- 用户已于 2026-03-16 对启动期“加载文件”路径追加反馈：当前启动期窗口里的抽象分阶段进度仍不够明确；建议改成按音频数量推进的可感知进度，例如 `0/12 -> 12/12`，每实际载入/分析一个音频文件就推进一格，以降低等待焦虑。
- 用户已于 2026-03-16 对“启动 Basic Filter”路径追加验收要求：局部转圈按钮不能在 realtime runtime 创建成功时就消失；只有当 `CABLE output` 已真正具备输出 `Basic Filter` 音频的能力时，这个局部转圈按钮才允许消失，否则用户会误以为 `CABLE output` 坏了。
- 用户已于 2026-03-16 新增执行约束：从现在开始 `cargo build` 不再由 Codex 主动执行，构建命令统一由用户手动执行；Codex 只可在回复中说明需要用户自行运行构建。
- 用户当前执行要求：整理本次新增功能，按功能块拆成几个 Git commit，并直接推送到远端仓库。

## 17. 当前关键决策（持续更新）

本节记录已经确认、不应轻易反复的决策。

- v1 不自研虚拟音频驱动，使用现成虚拟音频线。
- “训练”定义为 enrollment/calibration，而不是从零训练模型。
- 统一使用 ONNX Runtime 承载模型推理。
- 先做 `Passthrough` 与 `Basic Filter`，再做 `Strong Isolation`。
- v1 采用单用户单机模式，不提供角色切换或多 profile 选择，统一使用默认 profile。
- 模型默认路线：
  - VAD：Silero VAD ONNX
  - Speaker Embedding：WeSpeaker ONNX / ECAPA-TDNN 备选
  - Enhancement：GTCRN ONNX
  - TSE：WeSep ONNX（条件允许时集成）
- 实时链路采用回调线程 + ring buffer + worker 线程架构。
- 音频回调线程禁止做重型工作。
- 训练录音会话与 `Passthrough` 不并行抢占同一输入设备；进入训练录音前需先停止 `Passthrough`。
- 当前基础质量检查仅用于提示语音有效性与环境噪声，不向用户提示爆音风险或平均音量偏高，也不以错误结果阻塞后续步骤。
- Review 阶段的定向重录采用独立 2 步小流程；重录完成后自动返回第 25 步“完成确认”。
- 训练页“预览”默认走系统当前默认输出设备，用于本地试听，不复用 `VB-CABLE` 虚拟线输出。
- 默认 `speaker_profile.json` 当前先自动写入训练元数据与质量摘要，作为 metadata-only profile；后续接入真实 embedding 提取后再覆盖为正式声纹 profile。
- 启动期若检测到 `profiles/default/recordings` 下已有录音，允许用户直接加载到训练 Review 阶段；加载后按“刚录完”路径立即触发一次质量检查。
- 启动期弹窗里的“全部覆盖重录”也采用三连击防误触，避免误删旧录音目录。
- 在实际 WeSpeaker/ECAPA ONNX 模型文件接入前，M2 先用本地确定性启发式 embedding 抽象打通默认 profile 的 embedding 聚合、阈值估计和 UI 展示；后续模型落地后再替换为真实 speaker embedding。
- “加载旧录音”和“启动实时链路”的等待反馈必须就地展示在用户当前操作区域：前者用启动期窗口内进度条，后者用设备页启动按钮局部转圈；不再使用统一忙碌弹窗。
- “加载旧录音”和“启动实时链路”两条路径必须使用真实后台任务；UI 只负责显示分阶段进度和应用结果，不能再靠主线程同步执行重活伪装成加载。
- “启动实时链路”的局部 loading 不仅要存在于代码路径里，还必须在真机上稳定可见；必要时应引入立即重绘和最短可见时长，避免转圈按钮一闪而过或完全不可见。
- 启动期“加载文件”的进度反馈应优先使用按录音文件数量推进的可感知计数，而不是只显示抽象阶段文案；用户应能直接看到 `x/N` 形式的实际处理进度。
- “启动 Basic Filter”的局部 loading 结束条件必须从“runtime 创建成功”收紧为“输出链路真正就绪”；至少要等到 `CABLE output` 已实际进入可输出 `Basic Filter` 音频的状态，按钮才可退出 loading。
- 从现在开始 `cargo build` 由用户手动执行；除非用户后续明确撤回该要求，否则 Codex 不再主动运行任何 `cargo build` 验证命令。
- 当前工作区的未提交改动需要按“本次新增功能”做功能块整理后提交，不做单一大杂烩 commit；提交完成后直接推送远端。
- 本轮提交分组策略已确定为 3 组：1）离线算法/声纹评分/profile 升级；2）实时 `Basic Filter` runtime 与启动等待反馈；3）`AGENTS.md` 进度记忆与本轮执行约束更新。

## 18. 当前里程碑状态（持续更新）

用最简短方式标记进度。每次推进都要更新。

- [x] M0 工程初始化
- [x] M1 音频设备与直通
- [x] M2 训练/注册流程
- [x] M3 离线推理验证
- [ ] M4 实时 Basic Filter
- [ ] M5 Strong Isolation
- [ ] M6 工程化与稳定性

### 当前正在进行
- M4 实时 Basic Filter：已把“启动 Basic Filter”路径的 loading 结束条件从 runtime 创建成功收紧到输出链路真正就绪；当前等待用户真机复测局部转圈按钮是否能持续到 `CABLE output` 真正可输出 `Basic Filter` 音频。

### 下一步
- 让用户真机复测“启动 Basic Filter”路径，确认局部转圈按钮是否会持续到 `CABLE output` 真正可输出音频
- 若这条已达标，再继续观察是否还需要给这段等待增加更明确的“输出链路就绪”文案
- 若仍未达标，则继续补更强的就绪判定或超时/异常提示

## 19. 当前已完成内容（持续更新）

该节记录已完成的实质性工作；不得写空话。

- 已明确产品目标、硬约束、阶段性路线与 v1 范围。
- 已明确虚拟麦克风策略为“对接现成虚拟音频线”，不是自研驱动。
- 已明确训练定义为 enrollment/calibration。
- 已给出建议仓库结构、里程碑、模型路线与模块边界。
- 已要求开发过程中持续更新 `AGENTS.md`。
- 已读取并确认根目录 `AGENTS.md`，当前按 M0 顺序开始初始化仓库。
- 已创建 `rust-toolchain.toml`，固定为 `nightly-2025-07-12`。
- 已创建 `Cargo.toml`、`src/main.rs` 与 `src/` 下各模块占位文件。
- 已创建 `assets/`、`models/`、`profiles/`、`tools/model_prep/`、`tests/` 等基础目录骨架。
- 已添加 `assets/prompts_zh_cn.txt` 与 `models/manifest.json` 占位文件。
- 已接入 `eframe/egui` 原生窗口入口，并建立设备/训练/推理/调试四页基础 UI 骨架。
- 已接入 `tracing` 日志初始化。
- 已实现基于 `directories` + `serde_json` 的本地配置读写，默认保存到 Windows 配置目录下的 `settings.json`。
- 已通过 `cargo build` 验证 M0 最小工程可构建。
- 已执行短时 `cargo run` 启动检查，程序未在启动阶段立即报错退出。
- 已接入 `cpal`，可枚举默认 host 下的输入/输出设备并读取默认通道数与采样率。
- 设备页已支持刷新设备列表、显示输入/输出设备数量，并将所选设备名称持久化到配置文件。
- 已接入基于 `rtrb` ring buffer 的最小 `Passthrough` 实时链路。
- 已支持按所选输入/输出设备启动与停止实时音频通路，并在设备页显示运行状态、输入/输出峰值、丢帧/补零统计。
- 当前 `Passthrough` 内部统一为单声道 `f32`，支持不同 sample format 之间转换；若输入输出采样率不一致，会明确报错而不是隐式重采样。
- 最新代码已再次通过 `cargo build`，且短时启动检查未在 GUI 启动阶段立即报错。
- 已添加根目录 `.gitignore`，忽略 `target/` 构建产物。
- 已内置 `assets/fonts/SourceHanSansSC-Regular.otf`，并保留上游许可证文件 `assets/fonts/LICENSE-source-han-sans.txt`。
- 已补充内置 `assets/fonts/SourceHanSansSC-Bold.otf`，与现有思源黑体许可证文件保持一致。
- 已在 `egui` 启动阶段覆盖默认字体族，统一使用思源黑体 Regular 渲染 `Proportional` 与 `Monospace` 字体族。
- 字体改动后已再次通过 `cargo build`，且短时启动检查未在字体初始化阶段报错。
- 已确认当前版本只能把处理结果送到普通输出设备用于监听，尚未创建或暴露可被 OBS 选择的系统级虚拟麦克风设备。
- 已确认不修改 v1 路线：系统级虚拟麦克风 endpoint 继续依赖现成虚拟音频线/虚拟麦克风驱动，而不是本程序自建驱动。
- 已现场确认 `VB-CABLE` 已安装，当前 `Passthrough` 输出到播放端工作正常。
- 已实现 `VB-CABLE` 录音/播放端配对检测。
- 已在应用启动与设备刷新时优先把程序输出设备切到检测到的 `VB-CABLE` 播放端。
- 设备页已明确提示：程序输出使用 `VB-CABLE` 播放端，OBS / Discord 应选择对应录音端。
- 已现场确认 OBS 能从 `VB-CABLE` 录音端收到程序处理后声音，M1 验收通过。
- 已补全 `assets/prompts_zh_cn.txt` 固定短句内容，覆盖直播常用口播、数字、快慢语速与常见音素。
- 已实现训练脚本加载器，并在训练页展示内置固定短句列表。
- 已增加最小单元测试，验证内置中文提示词数量和内容非空。
- 已建立 `speaker_profile.json` 的数据结构与 `profiles/default/speaker_profile.json` 默认存储约定。
- 已移除 profile 标识输入与 profile 列表选择 UI，训练页改为展示默认 profile 状态。
- 已将内置中文固定短句资源收敛为 10 句，并把自由发挥要求固定为 30 秒。
- 已在训练页接入强引导状态机，按“训练准备 -> 环境静音 -> 10 句固定短句 -> 30 秒自由发挥 -> 完成确认”单步推进，不允许跳步。
- 训练准备页已要求先选择真实麦克风输入，未选输入设备时不允许开始训练。
- 已新增训练状态机测试与提示词数量测试，并通过验证。
- 强引导训练页改动后，`cargo build` 已再次通过。
- 已细化训练页引导：环境静音步骤改为倒计时显示，并补了固定短句开始前与自由发挥开始前的准备步骤。
- 已在训练页主确认按钮下方加入“有一段没录好，重新录制上一句话”按钮；在固定短句与自由发挥阶段可回退到上一条固定短句重录，前置步骤无上一句时按钮禁用。
- 回退上一句话逻辑已补充状态机测试，并再次通过 `cargo build` 验证。
- 已把“有一段没录好，重新录制上一句话”改为连续点击 3 次才真正回退，并在按钮下方显示防误触说明与当前确认次数。
- 已把环境静音步骤改为倒计时归零后自动进入固定短句准备，不再要求用户手动确认。
- 已在训练页统一显示“当前输入设备”和加粗麦克风录制状态；未录制时显示红色“当前麦克风未录制”，录制阶段显示绿色“当前麦克风录制中”。
- 自动跳转与录制状态逻辑已补充测试，并再次通过 `cargo build` 验证。
- 已把固定短句流程改成逐句准备：每一句固定短句前都有单独准备阶段，准备页直接展示该句文本。
- 固定短句录制页按钮文案已改为“确认本句已完成，进入下一个准备阶段”；最后一句结束后进入自由发挥准备。
- 逐句准备状态机测试已通过；当前代码已通过 `cargo check`。
- 再次执行 `cargo build`，确认逐句准备流程改动后的当前代码可成功构建。
- 已在 `egui` 中额外注册思源黑体 Bold 专用字体族，并让训练页麦克风状态文本显式使用 Bold 字体文件，而不是只依赖 `strong()`。
- Bold 字体接入后，`cargo build` 已再次通过。
- 已在麦克风状态文本左侧补固定宽度的自绘 icon：未录制显示暂停，录制中显示播放；两个 icon 共用一致的绘制尺寸，不会影响状态文字对齐。
- 状态 icon 接入后，`cargo build` 已再次通过。
- 已将麦克风状态行改为显式居中对齐布局，使播放/暂停 icon 与状态文本按水平中线对齐。
- 状态行对齐修正后，`cargo build` 已再次通过。
- 已修正状态行对齐回归：恢复原始左侧流式布局，只在固定宽度的 icon 槽位内按文本高度垂直居中绘制图标。
- 本轮状态行回归修正后，代码已通过 `cargo check`。
- 再次执行 `cargo build`，确认状态行回归修正后的当前代码可成功构建。
- 已把训练页“第 25 步：完成确认”中的“重新开始本轮训练”改为连续点击 3 次才真正丢弃本轮训练信息并重置流程。
- 已为三连击重置逻辑补充状态机测试，并再次通过 `cargo build` 验证。
- 已把训练流程的步进控制收口到应用层统一处理，用于协调训练录音的开始、结束保存和重录丢弃。
- 已接入基于 `cpal` + `rtrb` + worker 线程的真实训练录音会话，可实际录制环境静音、10 句固定短句和自由发挥音频。
- 已将训练录音保存为 `profiles/default/recordings/*.wav`，并在训练页显示环境静音 / 固定短句 / 自由发挥的录音完成情况。
- 已在训练页“当前麦克风未录制 / 当前麦克风录制中”右侧增加实时条形音量指示器，用于展示当前麦克风输入电平。
- 已在训练页加入训练录音错误可见提示，并要求开始训练前先停止设备页中的 `Passthrough`，避免输入设备占用冲突。
- 本轮真实录音会话接入后，`cargo test` 与 `cargo build` 已再次通过。
- 在收尾移除未用接口和更新完成确认文案后，当前代码已再次通过 `cargo build`。
- 已将 `固定短句：已录制 x/10 句` 改为可展开摘要，展开后会展示全部 10 句固定短句文本及各句录制状态。
- 本轮固定短句摘要交互改动后，代码已再次通过 `cargo build`。
- 已为主内容区接入统一的纵向滚动条，窗口高度不足时页面内容可上下滚动查看。
- 本轮主内容滚动条改动后，代码已再次通过 `cargo build`。
- 已修正主窗口面板顺序：底部状态栏现在先于 `CentralPanel` 创建，滚动到底部时不会再遮挡主内容区最后几行。
- 本轮底部遮挡修复后，代码已再次通过 `cargo build`。
- 已按功能块创建首批 Git 提交：工程骨架为 `chore: bootstrap rust desktop app scaffold`，应用功能实现为 `feat: add passthrough audio routing and guided training flow`，持续记忆文档为 `docs: add project constraints and progress log`。
- 已实现 `src/profile/quality.rs` 基础质量检查：会读取已落地的训练 WAV，按环境静音 / 固定短句 / 自由发挥计算环境噪声、RMS、活动时长、活动片段数与有效语音情况，并生成结构化质量报告。
- 已在训练完成进入 Review 时自动执行基础质量检查；重录上一句或重新开始训练时会清空旧质量报告，避免显示过期结果。
- 已在训练页接入基础质量检查结果展示：可显示整轮训练的警告/错误统计、环境静音电平、有效语音总时长，以及具体有问题的录音片段与原因。
- 本轮基础质量检查改动后，`cargo test` 与 `cargo check` 已通过；`cargo build` 因运行中的 `target/debug/ek-single-mic.exe` 占用输出文件失败，但不是代码编译错误。
- 已再次执行 `cargo build`，当前代码可成功完成构建；仍存在若干占位模块与未接入字段带来的 `dead_code` 警告，但不影响当前 M2 继续推进。
- 已按用户要求调整基础质检提示策略：不再向用户提示爆音风险或平均音量偏高；当判定有效语音不足时，不再叠加平均音量偏低提示。
- 已调整训练页 Review 与基础质检摘要文案：即使基础检查存在错误，也明确允许用户继续下一步操作，质检结果仅作提示不作阻塞。
- 本轮质检提示策略调整后，`cargo test` 与 `cargo build` 已再次通过。
- 已为 Review 阶段的“当前录音结果”接入定向操作按钮：环境静音 / 每句固定短句 / 自由发挥前都可显示“重录”“预览”，其中这里的“重录”需连续点击 3 次才真正触发。
- 已为“展开查看有问题的录音片段”里的固定短句接入“重录”“预览”按钮；这里的“重录”点击 1 次即可直接触发。
- 已扩展训练状态机与命令层，支持 Review 阶段的 2 步定向重录流程；固定短句重录时，上方强引导框会显示“第 1 步：重录 固定短句准备 xx/10”“第 2 步：重录 固定短句 xx/10”。
- 定向重录完成后，页面会自动回到“第 25 步：完成确认”，并重新生成该阶段的基础质检结果与完整 Review 信息。
- 已接入录音本地预览会话：点击“预览”会将对应 WAV 播放到系统默认输出设备，并在训练页显示当前预览状态与错误信息。
- 本轮 Review 定向重录/预览改动后，`cargo test` 与 `cargo build` 已通过。
- 已把“展开查看有问题的录音片段”里的操作范围补齐到环境静音 / 固定短句 / 自由发挥，三类问题片段现在都支持直接“重录”“预览”。
- 已把 Review 阶段的环境静音定向重录改为与主训练流程一致的 5 秒倒计时；倒计时结束后会自动返回“第 25 步：完成确认”。
- 本轮 Review 问题片段补齐与环境静音倒计时统一后，`cargo test` 与 `cargo build` 已再次通过。
- 已实现 `src/profile/build.rs` 的默认 profile 构建器：当前会基于训练录音清单和质量报告生成 metadata-only `speaker_profile.json`，写入训练元数据、质量摘要和原始录音路径引用。
- 已把默认 profile 的保存与重新加载接入训练 Review 闭环：进入“第 25 步：完成确认”后会自动刷新 `profiles/default/speaker_profile.json`，并立刻回读默认 profile 摘要到训练页。
- 已扩展默认 profile 摘要展示：训练页现在会显示模型版本、embedding 数量、质检摘要，并明确标注当前 profile 仍是 metadata-only，占位等待下一步 embedding 接入。
- 已为默认 profile 构建与存储补充单元测试；本轮 profile 写入闭环改动后，`cargo test` 与 `cargo build` 已再次通过。
- 已实现启动期默认录音目录扫描：程序启动时会检查 `profiles/default/recordings` 是否完整、是否存在杂项/损坏文件，并校验是否与 `profiles/default/speaker_profile.json` 的 `source_recordings` 一一对应。
- 已接入启动期录音弹窗：录音完整且与默认 profile 对应时显示已检测到旧录音提示；录音不完整、存在杂项/损坏文件或与默认 profile 不对应时显示警告。
- 已接入启动期“加载文件”动作：会把已识别的旧录音直接装入训练清单，跳到“第 25 步：完成确认”，并立即触发一次基础质量检查。
- 已接入启动期“全部覆盖重录”动作：会清空 `profiles/default/recordings` 目录并重置训练流程，且该按钮需要连续点击 3 次才会真正触发。
- 已为启动期录音扫描与三连击覆盖重录补充单元测试；本轮启动期检测/加载改动后，`cargo test` 与 `cargo build` 已再次通过。
- 已在 `src/ml/speaker.rs` 实现本地确定性启发式 speaker embedding 提取：当前会从固定短句与自由发挥 WAV 中提取零交叉率、频谱斜率代理、音高强度与音高归一化等帧级特征，并聚合为归一化 embedding。
- 已把 `src/profile/build.rs` 从 metadata-only 构建切换为真实 embedding 聚合：默认 `speaker_profile.json` 现在会写入 `embedding_count`、`embedding_dimension`、`centroid`、`dispersion` 和 `suggested_threshold`。
- 已扩展训练页与推理页默认 profile 展示：当默认 profile 已有 embedding 时，会明确显示“embedding 已就绪”、embedding 数量和建议阈值。
- 已为 speaker embedding 提取与 profile 聚合补充单元测试；本轮 embedding/profile 聚合改动后，`cargo test` 与 `cargo build` 已再次通过。
- 当前 M2 里程碑要求已满足：训练录音、质量检查、embedding 提取、默认 profile 保存/加载和默认 profile 状态展示均已闭环。
- 已实现离线 WAV 单声道读写与线性重采样基础设施：`src/pipeline/frames.rs` 现可读写 WAV、统一为内部 `mono/f32`，并在离线路径中显式重采样到 16 kHz 模型采样率。
- 已将 `src/ml/vad.rs` 从占位实现替换为启发式离线 VAD：按 25 ms 窗口、10 ms hop 和 hangover 规则输出逐帧活动判定，供离线 Basic Filter 使用。
- 已实现首版离线 `Basic Filter`：`src/pipeline/mod.rs` 现会基于默认 profile 的 centroid、建议阈值、speech activity threshold 和局部上下文 embedding 做 similarity gating，并通过平滑增益与软限幅生成输出 WAV。
- 已在 `src/ml/enhancement.rs` 接入离线增益平滑与逐帧叠加应用逻辑，在 `src/ml/speaker.rs` 增加启发式 match score，用于提升目标说话人与非目标说话人的离线区分度。
- 已为离线 `Basic Filter` 增加合成样本回归测试：当前会用训练样本构造 profile，并验证 target-like 语段保留强于 off-target 语段；本轮改动后 `cargo test` 已通过。
- 本轮离线 `Basic Filter` 接入后，`cargo build` 已再次通过；当前保留的构建警告主要来自尚未接入 UI/命令层的离线处理 helper 和既有占位模块。
- 已把离线 `Basic Filter` 入口接到调试页：现在可直接输入 WAV 路径、指定输出路径、调用默认 `speaker_profile.json` 处理离线音频，并在界面查看采样率、时长、活动帧、相似度阈值和平均增益等指标。
- 已在应用层新增离线处理命令与状态：调试页支持恢复默认路径，默认指向 `profiles/default/recordings/free_speech.wav` 和 `profiles/default/offline_outputs/free_speech_basic_filter.wav`，便于直接做第一轮离线验证。
- 已为调试页离线入口补充状态测试；本轮改动后 `cargo test` 25/25 通过。
- 已按功能块把当前 M2/M3 未提交工作整理为多条 Git 提交：分别覆盖训练录音/质检/profile 后端、离线 `Basic Filter` 后端，以及应用层/GUI 接线，便于后续审阅与回退。
- 已将当前整理后的全部 commit 推送到 `git@github.com:SEmmmer/EKSingleMic.git`，当前本地 `master` 已开始跟踪 `origin/master`。
- 已于 2026-03-15 重新读取根目录 `AGENTS.md` 并核对当前仓库基线：当前 Git 工作树干净，现正按里程碑顺序复核实际构建/测试状态。
- 已于 2026-03-15 再次执行实际校验：`cargo test --target-dir target/test-verify` 25/25 通过，`cargo build --target-dir target/build-verify` 通过；当前代码基线与 M2 完成、M3 进行中的文档状态一致。
- 已于 2026-03-15 核对本地 `profiles/` 目录：当前工作区未包含 `profiles/default/` 及其训练录音、默认 profile、离线输出样本，因此 M3 仍缺少真实样本验证记录，不能提前标记完成。
- 已于 2026-03-15 基于当前训练页/调试页实际交互整理 M3 样本采集操作说明：先停止 `Passthrough`，再按训练页强引导录制 `ambient_silence.wav`、`fixed_prompt_01.wav`~`fixed_prompt_10.wav`、`free_speech.wav`，随后在调试页运行离线 `Basic Filter` 生成默认输出样本。
- 用户已于 2026-03-15 完成真实样本采集与首轮离线验证：默认 `speaker_profile.json` 已顺利生成，额外录制了 `target_only.wav`、`crosstalk.wav`、`off_target.wav`，离线运行无报错；主观听感为目标说话人音量忽大忽小、非目标说话人抑制不足，默认离线输出位于 `offline_outputs/`，额外测试样本位于 `profiles/default/test/`。
- 已于 2026-03-15 确认真实样本与默认 profile 的当前实际落盘位置：文件不在仓库根目录 `profiles/default/`，而是在 `target/release/profiles/default/` 下，说明用户本轮是以 `target/release` 为工作目录运行程序。
- 已于 2026-03-15 基于真实样本完成第一轮量化分析：当前 profile 的 `suggested_threshold=0.9001`、`dispersion=0.0114`，但离线链路实际使用的 operating threshold 被推高到 `0.98`；训练样本对 centroid 的相似度实际只有 `0.8983~0.9679`，`target_only` 的活动帧平均增益仅约 `0.57`，`crosstalk` 约 `0.46`，`off_target` 约 `0.12`，已确认过高门限是目标说话人音量 pumping 的主要原因之一。
- 已于 2026-03-15 完成离线 `Basic Filter` 第一轮参数校准：将相似度上下文从 `0.24 s` 调整到 `0.40 s`，把 operating threshold 改为基于 `suggested_threshold + dispersion + 0.04` 且上限收敛到 `0.93`，同时收窄相似度过渡带并放慢增益衰减平滑；本轮新增阈值回归测试后，`cargo test --target-dir target/test-verify` 26/26 通过，`cargo build --target-dir target/build-verify` 通过。
- 已于 2026-03-15 用同口径启发式脚本重新扫过真实样本分布：本轮校准后，`target_only` 活动帧平均增益预计约 `0.63`，`free_speech` 预计约 `0.61`，高于首轮的约 `0.57` / `0.46`；`off_target` 仍预计维持在约 `0.12` 的低增益区间，后续需由用户重新导出实际 WAV 做第二轮听检确认。
- 用户已于 2026-03-15 要求由 Codex 直接批量导出第二轮试听文件，并在导出完成后返回这些 WAV 的绝对路径。
- 已于 2026-03-15 直接基于当前最新离线参数批量导出第二轮试听文件，保留原 `*_filter.wav` 不覆盖；新文件已写入 `target/release/profiles/default/offline_outputs/`，分别为 `target_only_filter_tuned.wav`、`crosstalk_filter_tuned.wav`、`off_target_filter_tuned.wav`、`free_speech_filter_tuned.wav`。
- 用户已于 2026-03-16 反馈第二轮听感：当前版本差异仍不明显，且 `crosstalk` 中当旁人被压下去时，用户本人声音也会一起被压下去；这进一步确认当前主要缺陷是重叠语音场景下的目标说话人保留不足。
- 已于 2026-03-16 在离线 `Basic Filter` 中加入目标存在保持/hysteresis：当前会在最近检测到目标说话人后，为后续短时间重叠语音保留一个目标增益下限，避免 `crosstalk` 中目标与串音一起被同步压低；本轮新增状态机测试后，`cargo test --target-dir target/test-verify` 27/27 通过，`cargo build --target-dir target/build-verify` 通过。
- 已于 2026-03-16 基于目标存在保持版本直接批量导出第三轮试听文件，不覆盖前两轮输出；新文件已写入 `target/release/profiles/default/offline_outputs/`，分别为 `target_only_filter_hold.wav`、`crosstalk_filter_hold.wav`、`off_target_filter_hold.wav`、`free_speech_filter_hold.wav`。
- 用户已于 2026-03-16 反馈第三轮听感：`hold` 版本里 `crosstalk` 中用户本人声音更稳了，但旁人声音也更容易被放出来；这说明“固定增益下限”的目标保持策略过宽，正在用串音泄漏换目标保留。
- 已于 2026-03-16 完成目标存在保持第二轮收紧：把 `hold` 期间的固定增益下限改为仅在 `similarity >= exit_threshold` 时生效的动态 floor，按 similarity 在 `MIN_ACTIVE_GAIN` 与保持下限之间插值，避免低分帧在 hold 期间被整体抬高；回归测试与状态机测试后，`cargo test --target-dir target/test-verify` 27/27 通过，`cargo build --target-dir target/build-verify` 通过。
- 已于 2026-03-16 基于动态 hold floor 版本直接批量导出第四轮试听文件，不覆盖前三轮输出；新文件已写入 `target/release/profiles/default/offline_outputs/`，分别为 `target_only_filter_hold_blend.wav`、`crosstalk_filter_hold_blend.wav`、`off_target_filter_hold_blend.wav`、`free_speech_filter_hold_blend.wav`。
- 用户已于 2026-03-16 反馈第四轮听感：`hold_blend` 版本里 `crosstalk` 的旁人声音比 `hold` 版更少一些，但用户本人声音没有继续保持住；这确认当前仅靠 hold/hysteresis 调参已经触到启发式 similarity gating 的上限。
- 已于 2026-03-16 完成离线 speaker score 升级：`src/ml/speaker.rs` 新增基于多条训练参考 embedding 的 support score，`src/pipeline/mod.rs` 现会在 centroid 分数之外引入 reference support 做 blended speaker score，而不是继续只靠单一 centroid 相似度；同时修正了参考 embedding 提取必须使用源录音真实采样率的问题。本轮新增 reference extraction / profile match 回归测试后，`cargo test --target-dir target/test-verify` 29/29 通过，`cargo build --target-dir target/build-verify` 通过。
- 已于 2026-03-16 直接基于多参考 speaker score 版本批量导出第五轮试听文件，不覆盖前四轮输出；新文件已写入 `target/release/profiles/default/offline_outputs/`，分别为 `target_only_filter_refscore.wav`、`crosstalk_filter_refscore.wav`、`off_target_filter_refscore.wav`、`free_speech_filter_refscore.wav`。
- 用户已于 2026-03-16 确认第五轮 `refscore` 版本“已经可以了”；M3 所需的离线 WAV 验证、主观听检和结果记录现已闭环，可正式进入 M4 实时 `Basic Filter`。
- 已于 2026-03-16 完成 M4 首个实时小步：`src/pipeline/realtime.rs` 已从单一 `PassthroughRuntime` 扩展为按 `InferenceMode` 启动的 `RealtimeRuntime`，并新增基于 ring buffer + worker 线程的 `BasicFilterRuntime`；实时 worker 会复用离线 `Basic Filter` 的 profile 预加载、VAD、reference-aware speaker score、目标存在保持与增益平滑逻辑，音频回调线程仍只负责轻量读写 ring buffer。
- 已于 2026-03-16 完成 M4 当前轮 UI/应用层接线：`src/app/mod.rs` 现在会按推理模式启动对应实时 runtime，设备页/推理页已能显示 `Basic Filter` 运行状态、当前 speaker score、当前增益和最近 chunk 的活动帧统计。本轮 `cargo test --target-dir target/test-verify` 29/29 通过，`cargo build --target-dir target/build-verify` 通过。
- 用户已于 2026-03-16 完成首轮 M4 真机反馈：当前实时 `Basic Filter` 可稳定启动、无明显延迟，目标说话人能保持住，旁人声音相较 `Passthrough` 更下去；说明首版实时 worker 已具备继续工程化打磨的基础。
- 已于 2026-03-16 完成首版等待反馈改造：`src/app/state.rs` 新增忙碌状态与延后一帧执行的 deferred command 机制，`src/app/mod.rs` 新增统一忙碌窗口，当前会在“加载旧录音”和“启动实时链路”前先显示进度条/加载提示，再执行实际命令；本轮新增忙碌状态测试后，`cargo test --target-dir target/test-verify` 30/30 通过，`cargo build --target-dir target/build-verify` 通过。
- 已于 2026-03-16 完成实时启动路径小优化：`src/pipeline/realtime.rs` 里的 `BasicFilterEngine::from_profile()` 已从主线程启动路径移动到 worker 线程内部，减少点击“启动 Basic Filter”后 UI 线程在 profile/reference embedding 初始化上的停顿。
- 已于 2026-03-16 按用户修正要求完成等待反馈第二轮改造：移除统一忙碌弹窗；`src/app/mod.rs` 现会在“检测到之前保存的录音”窗口内直接显示加载进度条，`src/ui/devices.rs` 现会在设备页把“启动实时链路”按钮切换为局部 `Spinner + disabled button`，`src/app/state.rs` 用 `BusyAction` 区分这两类就地忙碌状态；本轮再次通过 `cargo test --target-dir target/test-verify`（30/30）与 `cargo build --target-dir target/build-verify`。
- 已于 2026-03-16 完成等待反馈第三轮改造：移除上一轮 `deferred command` 伪异步做法，`src/app/mod.rs` 现通过后台线程 + `mpsc` 事件回传来异步执行“加载旧录音”和“启动实时链路”，并在主线程轮询应用结果；“加载旧录音”现在会按“恢复录音清单 -> 质量检查 -> 刷新默认 profile -> 同步 profile 摘要”阶段推进，“启动实时链路”现在会按“准备实时链路 -> 加载默认 profile -> 打开音频设备并创建实时链路 -> 同步实时状态”阶段推进；`src/ui/devices.rs` 额外在局部转圈按钮下显示阶段文案和进度条，并在忙碌期间禁用设备相关交互。本轮 `cargo fmt`、`cargo test --target-dir target/test-verify`（30/30）和 `cargo build --target-dir target/build-verify` 均已通过。
- 已于 2026-03-16 完成启动期进度条第四轮改造：`src/profile/quality.rs` 新增可带回调的 `analyze_manifest_with_progress(...)`，`src/profile/record.rs` 新增 `recorded_clip_count()`，`src/app/mod.rs` 现会在“加载文件”后台任务里按已分析录音文件数量持续上报进度，把启动期窗口内进度从抽象阶段文案改为 `正在载入训练音频 x/N`，并在进度条文本中直接显示该计数；默认 profile 刷新与摘要同步仍保持异步阶段，但不再掩盖前半段按录音数量推进的感知进度。本轮 `cargo fmt`、`cargo test --target-dir target/test-verify`（31/31）和 `cargo build --target-dir target/build-verify` 均已通过。
- 已于 2026-03-16 完成实时启动等待第五轮改造：`src/pipeline/realtime.rs` 新增输出链路就绪所需的运行指标，当前会追踪 `successful_output_frames` 与 `processed_output_chunks`；`src/app/mod.rs` 现会在 realtime runtime 创建成功后继续保持 `StartRealtime` 忙碌状态，直到输出设备已消费到 `Basic Filter` 启动后产生的输出帧，再结束 loading；`src/ui/devices.rs` 现优先以 `StartRealtime` 忙碌状态决定是否显示局部转圈按钮，不再被 `RunningBasicFilter` 状态覆盖。本轮 `cargo fmt`、`cargo test --target-dir target/test-verify`（31/31）通过；由于运行中的 `ek-single-mic.exe` 占用了 `target/build-verify` 输出文件，构建验证改用独立目录 `target/build-verify-2` 并已成功通过。

## 20. 当前阻塞与待确认事项（持续更新）

若无阻塞，写 无。若有，写清楚原因与影响。

- 暂无代码层阻塞。
- 当前 `cargo build` 仍有占位模块的 `dead_code` 警告，但不影响 M0 完成与后续 M1 开发。
- 当前不升级为驱动工程；仍需用户在 Windows 中安装可用的虚拟音频线或虚拟麦克风驱动，程序再把音频送入其播放端。
- 当前主要风险已切换到 M3：离线 Basic Filter 链路已接通，但目前只在合成样本回归测试上验证了 target-like/off-target 的相对抑制效果，尚未完成真实离线 WAV 的主观听检和指标记录。
- 当前 M3 已完成离线 WAV 输入输出、启发式 VAD、speaker similarity gating、增益平滑和调试页离线入口；下一步是补齐真实样本验证、输出导出与结果记录，再推进实时 `Basic Filter`。
- 当前 `cargo build` 已恢复可成功完成；现阶段仅剩占位模块导致的 `dead_code` 警告，不构成代码阻塞。
- 当前首版离线链路新增了一批尚未接入 UI/命令层的 helper，本轮 `cargo build` 仍有 `dead_code` 警告；这些警告不阻塞 M3，但后续接入离线入口时应顺手收敛。
- 2026-03-15 复核结果显示：当前 `cargo test` 与独立目标目录 `cargo build` 均可稳定通过；现阶段代码阻塞仍主要集中在 M3 尚未沉淀真实样本听检记录，而非构建/测试失败。
- 2026-03-15 目录复核结果显示：当前本地仓库未保留 `profiles/default/` 训练录音、默认 profile 或离线输出文件，因此真实样本离线验证仍缺输入数据与结果归档，M3 不能按完成处理。
- 2026-03-15 用户反馈显示：真实样本首轮离线结果已能生成，但当前启发式 `Basic Filter` 存在明显音量 pumping，且对近距离串音的抑制不足；M3 的主要问题已从“缺少真实样本”转为“真实样本效果不达标且当前工作区尚未确认这些样本的实际落盘位置”。
- 2026-03-15 已确认当前工作区与运行期输出目录不一致：真实样本实际保存在 `target/release/profiles/default/`，后续分析与复现实验需直接针对该目录，且后续应考虑把默认录音/profile 路径从进程工作目录解耦，避免调试与发布目录各自产生一套数据。
- 2026-03-15 真实样本量化分析显示：当前离线链路对 `off_target` 已有一定抑制，但 `target_only` 与 `crosstalk` 的活动帧分数分布重叠明显；当前优先级应先修正过高 operating threshold 与过快的衰减平滑，缓解目标说话人忽大忽小，再继续评估启发式 speaker score 对近距离串音的上限。
- 2026-03-15 当前已完成第一轮参数校准，但 `crosstalk` 与 `target_only` 的分数分布仍有明显重叠；若第二轮听检后串音仍明显压不下去，则需继续评估更强的 speaker score / 条件过滤策略，而不是只靠当前启发式 centroid gating。
- 2026-03-16 第二轮听检反馈显示：仅靠门限/平滑校准仍不足以处理 `crosstalk` 下的目标保留问题；下一步应优先引入目标存在保持/hysteresis，减少目标与串音一起被同步压低的情况。
- 2026-03-16 第三轮听检反馈显示：目标存在保持/hysteresis 的固定 floor 虽能改善目标说话人的稳定性，但会同步放松 `crosstalk` 中的非目标抑制；当前离线链路已进入“目标保留”和“串音泄漏”之间的精细 tradeoff 阶段。
- 2026-03-16 第四轮听检反馈显示：把固定 floor 收紧为动态 floor 后，只能换来“串音少一点，但目标又不稳”；继续小调 hold 参数的收益已明显下降，后续应转向更强的 speaker score / 条件过滤策略。
- 2026-03-16 当前已确认：仅靠启发式 hold/hysteresis 无法同时满足目标保留与串音抑制，必须依赖更强的 speaker score 继续拉开 `target_only` 与 `crosstalk` 的分数分布；若多参考 score 仍不足，则 M3 后续需要尽早转向更强的条件过滤/TSE 路线。
- 2026-03-16 当前主要阻塞已从离线效果转移到实时架构：现有 `pipeline/realtime.rs` 仍只有 `Passthrough`，M4 需要在不把重型逻辑放进音频回调线程的前提下接入 worker 化的 `Basic Filter`。
- 2026-03-16 当前实时 `Basic Filter` 仍是首版 worker 实现：内部采用 chunk 级线性重采样与处理，且仍要求输入/输出设备采样率一致；真机上仍需重点观察延迟、chunk 边界抽动和长时间运行稳定性。
- 2026-03-16 当前等待反馈已升级为真正异步后台任务 + 分阶段进度；下一步风险不再是“主线程同步卡顿”，而是需要用户在 Windows 真机上确认跨线程启动音频设备是否稳定、阶段文案是否清晰、以及是否仍需要取消/超时保护。
- 2026-03-16 真机新增可见性问题已确认：即使后台任务已异步化，若点击后没有立即请求重绘、且任务完成过快，设备页仍可能完全看不到局部转圈按钮；当前需要补“立即重绘 + 最短可见时长”来保证这段反馈真正被用户看见。
- 2026-03-16 启动期进度可感知性问题已切到更具体层面：当前已把“加载文件”进度改为按录音文件数量推进的 `x/N`；剩余待确认点是这种文件计数式进度是否已足够明确，以及 profile 刷新尾段是否仍需继续细化。
- 2026-03-16 当前实时启动等待的剩余关注点已切换为“就绪判定是否足够贴近用户感知”：当前 loading 会保持到输出链路真正 ready，但仍需用户真机确认这是否已经与 OBS / `CABLE output` 的实际可用时刻足够一致。
- 若 `target/debug/ek-single-mic.exe` 正在运行，默认 `cargo build` 仍可能因 Windows 文件占用失败；本轮已用独立 `target/build-verify` 目录完成等价构建验证，不构成代码阻塞。
- 后续模型集成时，需要确认实际使用的 ONNX 模型文件、shape 与 license。
- 若 Windows 真机上音频设备枚举或格式兼容出现问题，应优先保证直通链路可用，再处理模型接入。

## 21. 变更日志（持续追加，不要删除历史）

每次有意义的工作完成后，追加一条新记录。格式尽量统一。

### 2026-03-11
- 初始化 `AGENTS.md`
- 固化项目目标、范围、硬约束、选型路线、里程碑顺序、更新规则
- 明确要求：开发过程中所有新的要求/重要记忆/新需求/已完成内容都必须同步更新到本文件
- 读取仓库根目录 `AGENTS.md` 并确认从 M0 开始执行
- 记录当前执行约束：不跳步，且每完成一个有意义步骤都先更新 `AGENTS.md`
- 完成 M0 第 1 步：固定 Rust toolchain，初始化 Cargo 工程与目录/模块骨架
- 完成 M0 第 2 步：接入 `eframe/egui` 窗口、`tracing` 初始化与基础配置持久化
- 完成 M0 第 3 步：`cargo build` 成功，短时启动检查未在启动阶段报错，M0 完成
- 完成 M1 第 1 步：接入 `cpal` 设备枚举、设备刷新与设备选择 UI
- 完成 M1 第 2 步：接入基于 `rtrb` 的 `Passthrough` 输入/输出 stream、运行状态与基础电平统计
- 补充基础工程文件 `.gitignore`，忽略 `target/`
- 记录新增要求：内置思源黑体 Regular，统一用于程序全部 UI 字体渲染
- 完成中文字体修复：内置思源黑体 Regular，并在 `egui` 中统一替换默认字体族
- 记录用户现场联调结论：当前实现只做到输入采集和输出监听，未向系统暴露虚拟麦克风，OBS 无法选择本程序作为音频输入
- 记录新增系统级需求：程序启动时创建可被 OBS / Discord 选择的虚拟麦克风设备
- 记录最终决策：保留原 v1 路线，继续依赖现成虚拟音频线/虚拟麦克风驱动，不自研系统级虚拟麦克风驱动
- 记录现场验证结果：`VB-CABLE` 已安装，当前 `Passthrough` 输出到播放端功能正常
- 完成接管 `VB-CABLE` 的工程化小步：自动识别播放端/录音端配对，并在 UI 中提示 OBS / Discord 应选择录音端
- 记录现场验收结果：OBS 已能收到处理后声音，M1 完成
- 完成 M2 第 1 步：落地固定短句脚本、训练页展示与提示词加载测试
- 完成 M2 第 2 步：建立 `speaker_profile.json` 存储骨架，并在训练页展示现有 profile 列表
- 记录新增约束：改单人单机固定使用，去掉 profile 切换，改为默认 profile
- 完成 M2 第 3 步：移除 profile 切换 UI 与配置项，收敛到默认 profile 模式
- 记录新增训练约束：训练页改为强引导逐步确认流程，固定短句缩短为 10 句，自由发挥固定为 30 秒
- 完成 M2 第 4 步：将固定短句资源收敛为 10 句，并把训练页改为单步确认推进的强引导状态机
- 完成提示词测试与训练状态机验证；本轮 `cargo build` 因运行中的本地程序占用输出文件失败，已使用 `cargo check` 验证当前代码可编译
- 再次执行 `cargo build`，确认强引导训练页改动后的当前代码可成功构建
- 完成训练页细化：环境静音改为倒计时显示，补充固定短句准备步骤与自由发挥准备步骤；状态机扩展并通过测试，当前代码已通过 `cargo check`
- 完成训练页回退能力：新增“重新录制上一句话”按钮与状态回退逻辑，并再次通过状态机测试与 `cargo build`
- 完成训练页行为补强：环境静音倒计时结束后自动进入固定短句准备，并新增当前麦克风录制状态展示；相关测试与 `cargo build` 已通过
- 完成固定短句逐句准备流程：每句前增加准备阶段、准备页展示该句文本、录制页按钮改为进入下一个准备阶段；状态机测试通过，当前代码已通过 `cargo check`
- 再次执行 `cargo build`，确认逐句准备流程改动后的当前代码可成功构建
- 完成状态文本字重修复：内置思源黑体 Bold，并让训练页麦克风状态文本显式使用 Bold 字体；`cargo build` 已通过
- 完成麦克风状态 icon：左侧增加固定宽度的播放/暂停自绘图标，并再次通过 `cargo build`
- 完成麦克风状态行对齐修正：播放/暂停 icon 与状态文本按水平中线居中对齐，并再次通过 `cargo build`
- 修正状态行布局回归：撤销影响整句位置的布局方式，改为只在 icon 槽位内按文本高度居中；当前代码已通过 `cargo check`
- 再次执行 `cargo build`，确认状态行布局回归修正后的当前代码可成功构建
- 记录新增完成确认要求：训练页“第 25 步：完成确认”中的“重新开始本轮训练”必须连续点击 3 次，才允许丢弃本轮训练信息并重新开始
- 完成训练页完成确认防误触：将“重新开始本轮训练”改为三连击确认重置，补充状态机测试并再次通过 `cargo build`
- 记录新增 M2 要求：训练流程接入真实录音会话，并在训练页状态行右侧增加条形音量指示器
- 完成 M2 当前小步：接入真实训练录音会话，按段保存 `profiles/default/recordings/*.wav`，训练页新增实时音量条、录音结果摘要与录音错误提示；`cargo test` 与 `cargo build` 已通过
- 收尾清理本轮改动：移除未使用接口、更新训练页完成确认文案，并再次确认 `cargo build` 通过
- 记录新增训练页摘要要求：固定短句录音统计改为可展开按钮，展开后展示全部短句文本与录制状态
- 完成训练页固定短句摘要交互：录音结果区已支持展开查看 10 句固定短句，并再次确认 `cargo build` 通过
- 记录新增窗口交互要求：主内容区增加可上下滚动的纵向滚动条，长页面不能被窗口高度截断
- 完成主内容区滚动支持：设备页/训练页/推理页/调试页已统一接入纵向滚动条，并再次确认 `cargo build` 通过
- 记录新增窗口布局修复要求：滚动到底部时，底部状态栏不能遮挡主内容区最后几行内容
- 完成主窗口布局修复：调整 `egui` 面板创建顺序，底部状态栏不再覆盖可滚动内容，并再次确认 `cargo build` 通过
- 记录新增训练页防误触要求：“有一段没录好，重新录制上一句话”按钮也必须连续点击 3 次后才允许真正回退生效
- 完成训练页回退防误触：将“重新录制上一句话”改为三连击确认，补充状态机测试并再次确认 `cargo build` 通过
- 记录新增仓库管理要求：将当前全部工作按功能块拆成几个 Git commit，并写清楚 commit message
- 完成首批 Git 历史拆分：已按功能块创建工程骨架提交、应用功能提交与持续记忆文档提交
- 完成 M2 当前小步：接入基础质量检查模块，按已落地 WAV 生成环境静音/固定短句/自由发挥的结构化质量报告，并在训练页展示整轮警告、错误与问题片段；`cargo test` 与 `cargo check` 已通过，`cargo build` 因运行中的 `ek-single-mic.exe` 占用输出文件失败
- 按用户要求再次执行 `cargo build`，当前构建已成功完成；保留的 `dead_code` 警告主要来自尚未接入的占位模块，不影响继续推进 M2
- 记录新增基础质检要求：不向用户提示爆音风险或平均音量偏高；有效语音不足时不叠加平均音量偏低；基础检查有错误也不能阻止继续下一步
- 完成基础质检提示策略调整：移除爆音风险与平均音量偏高提示，避免“有效语音不足”时叠加低音量提示，并在训练页明确标注基础检查不阻塞后续步骤；`cargo test` 与 `cargo build` 已通过

### 2026-03-12
- 记录新增 Review 交互要求：当前录音结果里的环境静音 / 固定短句 / 自由发挥增加“重录”“预览”按钮，其中摘要区“重录”需三连击确认
- 记录新增问题片段交互要求：有问题的固定短句前增加“重录”“预览”按钮，这里的“重录”单击即可触发
- 记录新增定向重录流程要求：固定短句重录使用 2 步小流程，并在完成后自动返回第 25 步“完成确认”
- 完成 Review 阶段定向重录：扩展训练状态机、命令层与训练页，让摘要区/问题片段区可以直接回到目标录音的重录准备和重录步骤
- 完成录音本地预览：训练页可直接试听已落地的 WAV，预览走系统默认输出设备，不复用 `VB-CABLE`
- 完成 Review 交互验证：`cargo test` 与 `cargo build` 已通过
- 记录新增 Review 细化要求：有问题的录音片段里的环境静音与自由发挥也必须提供“重录”“预览”入口
- 记录新增环境静音一致性要求：Review 定向重录环境静音时也必须显示 5 秒倒计时
- 完成 Review 问题片段入口补齐：环境静音 / 固定短句 / 自由发挥的问题片段都可直接重录或预览
- 完成环境静音定向重录倒计时统一：Review 中的环境静音重录改为 5 秒倒计时并自动返回完成确认
- 完成本轮验证：`cargo test` 与 `cargo build` 已再次通过
- 完成默认 profile 写入闭环：新增 `profile/build.rs` metadata-only 构建器，并在 Review 阶段自动写入/回读 `profiles/default/speaker_profile.json`
- 完成默认 profile 状态展示扩展：训练页会显示模型版本、embedding 数量和质检摘要，并明确当前 profile 仍待 embedding 接入
- 完成本轮 profile 闭环验证：新增 profile 构建/存储测试，`cargo test` 与 `cargo build` 已再次通过
- 记录新增启动期录音检测要求：程序启动时需检查 `profiles/default/recordings` 是否完整，并校验其是否与默认 `speaker_profile.json` 一一对应
- 记录新增启动期交互要求：启动期需提供“加载文件”和“全部覆盖重录”两个动作；“加载文件”要直接跳到第 25 步并触发一次质量检查
- 记录新增启动期防误触要求：弹窗里的“全部覆盖重录”也必须三连击后才允许真正触发
- 完成启动期旧录音检测：启动时会扫描默认录音目录，区分“完整且与默认 profile 对应”与“缺失/杂项/不对应”的两类提示
- 完成启动期旧录音加载：加载后会直接进入训练 Review，并按刚录完录音那样触发一次基础质量检查
- 完成启动期覆盖重录：可一键清空默认录音目录并重置训练流程，且按钮已接入三连击防误触
- 完成本轮启动期检测/加载验证：新增目录扫描与三连击状态测试，`cargo test` 与 `cargo build` 已再次通过
- 记录临时实现决策：在实际 WeSpeaker/ECAPA ONNX 模型文件接入前，M2 先用本地确定性启发式 embedding 抽象打通默认 profile 的 embedding 聚合与阈值估计
- 完成启发式 speaker embedding 提取：新增 `src/ml/speaker.rs`，可从固定短句与自由发挥录音生成归一化 embedding 并聚合 centroid/dispersion/threshold
- 完成默认 profile 正式聚合：`speaker_profile.json` 不再停留在 metadata-only，而是实际写入 embedding 数量、维度、centroid、dispersion 和建议阈值
- 完成默认 profile 就绪状态展示：训练页与推理页都会显示默认 profile 的 embedding 就绪状态与建议阈值
- 完成 M2 收尾验证：新增 embedding/profile 聚合测试，`cargo test` 与 `cargo build` 已再次通过，M2 完成并切换到 M3
- 完成 M3 首个离线子步骤：新增离线 WAV 读写、显式重采样、启发式 VAD、speaker similarity gating 与平滑增益应用，打通首版离线 `Basic Filter`
- 完成离线 profile 匹配增强：为启发式 embedding 增加 feature-aware match score，并把 `speech_activity_threshold_dbfs` 写入默认 `speaker_profile.json`
- 完成离线回归测试：新增合成 target/off-target 语段验证，`cargo test` 已通过
- 完成 M3 当前轮构建验证：`cargo build` 已再次通过；当前保留警告主要来自离线处理 helper 尚未接入 UI/命令层和既有占位模块
- 完成 M3 调试页离线入口：新增输入/输出 WAV 路径、恢复默认路径、运行离线 Basic Filter 按钮和指标展示，默认走 `profiles/default/speaker_profile.json`
- 完成 M3 当前轮命令层接线：应用层已支持直接调用离线 Basic Filter 处理默认 profile 下的 WAV 文件，避免真实样本验证仍只能依赖测试代码
- 完成本轮验证：`cargo test` 25/25 通过；默认 `cargo build` 因运行中的 `target/debug/ek-single-mic.exe` 被 Windows 占用失败，随后使用 `cargo build --target-dir target/build-verify` 完成独立构建验证
- 完成当前轮 Git 历史整理：已将 M2/M3 相关改动按功能块拆成多条 commit，并为每条 commit 写清晰的 `commit -m` 信息
- 完成当前轮远端同步：已新增 `origin -> git@github.com:SEmmmer/EKSingleMic.git` 并成功执行 `git push -u origin master`

### 2026-03-15
- 重新读取仓库根目录 `AGENTS.md`，按既定约束从当前里程碑状态开始复核
- 核对当前 Git 工作树：无未提交改动，适合继续做构建/测试与里程碑确认
- 执行实际基线校验：`cargo test --target-dir target/test-verify` 25/25 通过，`cargo build --target-dir target/build-verify` 通过；确认 M0/M1/M2 已稳定落地，M3 当前处于离线真实样本验证前的可构建状态
- 核对本地 `profiles/` 目录：当前仓库不含 `profiles/default/` 训练录音、默认 profile 或离线输出样本；确认 M3 尚缺真实样本听检与结果记录，不能跳到 M4
- 基于当前实现整理 M3 样本采集说明：明确用户需先在训练页生成默认录音与 `speaker_profile.json`，再去调试页运行离线 `Basic Filter`，补齐真实样本与结果文件
- 记录用户首轮真实样本验证结果：默认 profile 已生成，`target_only.wav` / `crosstalk.wav` / `off_target.wav` 已录制，离线处理无报错；当前主观问题是目标说话人音量忽大忽小、非目标说话人抑制不足，后续需结合真实样本指标继续排查
- 定位运行期输出目录：已在 `target/release/profiles/default/` 下找到 `speaker_profile.json` 与 `test/*.wav`，确认本轮真实样本不是缺失，而是写入了 `target/release` 工作目录
- 结合真实样本做第一轮量化诊断：确认当前离线链路把 `suggested_threshold=0.9001` 推到了 `operating_threshold=0.98`，高于多数训练样本相似度；接下来先针对离线 `Basic Filter` 做阈值/平滑校准，解决目标说话人音量 pumping
- 完成离线 `Basic Filter` 第一轮参数校准：调整相似度上下文、operating threshold 上限、相似度过渡带与增益平滑参数，并新增 operating threshold 回归测试；`cargo test --target-dir target/test-verify` 与 `cargo build --target-dir target/build-verify` 已再次通过
- 基于真实样本同口径重扫得到第二轮预估：本轮参数校准预计可把 `target_only` / `free_speech` 的平均活动增益从首轮的约 `0.57` / `0.46` 提升到约 `0.63` / `0.61`，而 `off_target` 仍保持低增益；下一步需要用户重新导出实际离线输出并做第二轮听检
- 记录新增执行要求：由 Codex 直接基于当前离线参数批量导出第二轮试听文件，并返回每个输出 WAV 的绝对路径给用户直接试听
- 直接导出第二轮试听文件：已在 `target/release/profiles/default/offline_outputs/` 下生成 `*_filter_tuned.wav` 四个新输出，供用户直接听感对比，不覆盖上一轮 `*_filter.wav`

### 2026-03-16
- 记录第二轮听检反馈：当前 `crosstalk` 中旁人被压下去时，用户本人声音也会一起被压下去；问题已明确收敛到重叠语音场景下的目标保留不足
- 决定推进新的离线小步：在 `Basic Filter` 中增加目标存在保持/hysteresis，而不是继续只靠单帧 similarity 门控
- 完成离线目标存在保持/hysteresis 改造：新增目标保持状态机与对应测试，`cargo test --target-dir target/test-verify` 27/27 通过，`cargo build --target-dir target/build-verify` 通过
- 直接导出第三轮试听文件：已在 `target/release/profiles/default/offline_outputs/` 下生成 `*_filter_hold.wav` 四个新输出，供用户重点对比 `crosstalk` 中目标说话人的保留情况
- 记录第三轮听检反馈：`hold` 版本里用户本人声音更稳，但 `crosstalk` 中旁人声音也更容易被放出来，说明固定 hold floor 过宽
- 完成动态 hold floor 收紧：仅在 `similarity >= exit_threshold` 时保留目标下限，并按 similarity 做 floor 插值，避免低分帧在 hold 期间被整体抬升；对应测试与构建验证已再次通过
- 直接导出第四轮试听文件：已在 `target/release/profiles/default/offline_outputs/` 下生成 `*_filter_hold_blend.wav` 四个新输出，供用户重点对比 `crosstalk` 中目标保留与串音泄漏的 tradeoff
- 记录第四轮听检反馈：`hold_blend` 版本确实让 `crosstalk` 的旁人声音比 `hold` 版更少，但目标说话人稳定性没有继续保持；决定停止继续小调 hold 参数，下一步切到更强的 speaker score / 条件过滤方向
- 完成离线 speaker score 升级：把离线 `Basic Filter` 从“当前帧对单一 centroid 打分”改为“centroid + 多条训练参考 embedding 支撑”混合打分，并补上参考 embedding 提取必须使用真实录音采样率的修正；新增相关测试后，`cargo test --target-dir target/test-verify` 29/29 通过，`cargo build --target-dir target/build-verify` 通过
- 直接导出第五轮试听文件：已在 `target/release/profiles/default/offline_outputs/` 下生成 `*_filter_refscore.wav` 四个新输出，供用户重点对比新 speaker score 对 `target_only` 稳定性和 `crosstalk` 串音抑制的影响
- 记录第五轮听检结论：用户确认 `refscore` 版本“已经可以了”；M3 离线推理验证达成当前验收标准，下一步切换到 M4 实时 `Basic Filter`
- 完成 M4 首个实时小步：把实时链路从固定 `Passthrough` 重构为按推理模式启动的 `RealtimeRuntime`，新增 `BasicFilterRuntime` worker 线程，并将设备页/推理页状态展示扩展到 `Basic Filter` 的当前分数、增益和最近 chunk 活动帧统计
- 完成 M4 当前轮构建验证：`cargo test --target-dir target/test-verify` 29/29 通过，`cargo build --target-dir target/build-verify` 通过；下一步进入 Windows 真机实时听检
- 记录首轮 M4 真机听检结果：实时 `Basic Filter` 可稳定启动、无明显延迟，目标说话人能保持住，旁人声音较 `Passthrough` 有一定压制；但“加载旧录音”和“启动 `Basic Filter`”时存在短暂卡顿，下一步补进度提示/加载反馈
- 完成首版等待反馈改造：为“加载旧录音”和“启动实时链路”增加统一的忙碌窗口与进度条，并把这两类命令改成“先展示提示、下一帧再执行”的 deferred command 流程
- 完成实时启动小优化：把 `BasicFilterEngine` 初始化从主线程搬到 worker 线程，减少点击“启动 Basic Filter”后的主线程阻塞
- 完成本轮验证：`cargo test --target-dir target/test-verify` 30/30 通过，`cargo build --target-dir target/build-verify` 通过；下一步请用户复测等待期体感
- 记录用户对等待反馈的修正要求：统一忙碌弹窗方案不合格；下一步改为“检测到之前保存的录音”窗口内进度条，以及“启动实时链路”按钮局部转圈状态，未达可用前不交还用户
- 完成等待反馈第二轮改造：移除统一忙碌弹窗，把“加载旧录音”改为启动期窗口内局部进度条，把“启动实时链路”改为设备页局部转圈按钮，并再次通过 `cargo test --target-dir target/test-verify`（30/30）与 `cargo build --target-dir target/build-verify`
- 记录用户新增要求：局部进度提示仍不足，下一步把“加载旧录音”和“启动实时链路”两条路径升级为真正异步后台任务，并提供分阶段进度回报
- 完成等待反馈第三轮改造：把“加载旧录音”和“启动实时链路”两条路径都改成真正异步后台任务，使用后台线程 + `mpsc` 事件回传分阶段进度；设备页局部 loading 区域新增阶段文案和进度条，忙碌期间设备交互被禁用；`cargo fmt`、`cargo test --target-dir target/test-verify`（30/30）与 `cargo build --target-dir target/build-verify` 均通过
- 记录用户新增建议：启动期“加载文件”路径应按音频数量推进，用 `0/12 -> 12/12` 形式让用户明确感知实际进度
- 完成启动期进度条第四轮改造：把“加载文件”路径改为按已分析录音数量推进的 `x/N` 进度，并在进度条文本中直接显示计数；`cargo fmt`、`cargo test --target-dir target/test-verify`（31/31）与 `cargo build --target-dir target/build-verify` 均通过
- 记录用户新增验收要求：局部转圈按钮必须持续到 `CABLE output` 真正具备输出 `Basic Filter` 音频能力，不能在 runtime 创建成功时就消失
- 完成实时启动等待第五轮改造：把 loading 结束条件从“runtime 创建成功”收紧到“输出链路真正 ready”，并让设备页在 `StartRealtime` 忙碌期间始终优先显示局部转圈按钮；`cargo fmt`、`cargo test --target-dir target/test-verify`（31/31）通过，等价构建验证改用 `cargo build --target-dir target/build-verify-2`
- 记录用户新增执行约束：从现在开始不再由 Codex 主动执行 `cargo build`，后续构建统一由用户手动运行
- 记录用户当前执行要求：整理本次新增功能并按功能块拆分 Git commit，随后直接推送远端
- 记录本轮提交分组决策：按“离线算法升级 / 实时 Basic Filter 与等待反馈 / AGENTS 记忆更新”三组提交
- 已完成第 1 组提交：离线算法/声纹评分/profile 升级已整理为 Git commit `c7375c3`（`feat: improve offline basic filter scoring`）
- 已完成第 2 组提交：实时 `Basic Filter` runtime 与启动等待反馈已整理为 Git commit `9a188b7`（`feat: add realtime basic filter startup feedback`）
- 已完成第 3 组提交：`AGENTS.md` 进度记忆与执行约束更新已整理为 Git commit `910e0b8`（`docs: update AGENTS for M4 progress and workflow constraints`）
- 已完成本轮远端推送：`c7375c3`、`9a188b7`、`910e0b8` 已于 2026-03-16 推送到 `origin/master`（`git@github.com:SEmmmer/EKSingleMic.git`）

## 22. 每次提交前检查清单

在结束当前一轮工作前，必须检查：

- [ ] 是否仍符合 Windows 10/11 + Rust nightly-2025-07-12 + eframe/egui 约束
- [ ] 是否没有偏离“现成虚拟音频线”策略
- [ ] 是否没有把“训练”误做成从零训练深度模型
- [ ] 是否没有在音频回调线程放入重型逻辑
- [ ] 是否更新了 `AGENTS.md`
- [ ] 是否同步更新了里程碑状态
- [ ] 是否记录了新决策/新风险/已完成内容
- [ ] 是否保持项目仍然可构建、可运行或至少方向一致

## 23. 默认执行口令

如果没有用户额外说明，默认按以下方式执行：

- 从 M0 开始
- 不跳步
- 每次实现一个清晰的小目标

每个小目标完成后：
- 更新 `AGENTS.md`
- 再继续下一步

如果用户提出新的要求，先更新本文件，再改代码。
