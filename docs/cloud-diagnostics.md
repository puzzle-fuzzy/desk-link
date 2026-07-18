# DeskLink 脱敏云端诊断

DeskLink 的云端诊断是用户主动开启的支持能力，不是远程控制业务通道，也不改变 Noise 端到端加密边界。默认关闭；关闭时客户端不会访问诊断服务。

## 上传内容与隐私边界

客户端只发送本机已经脱敏的连接生命周期事件，例如连接阶段、重试次数、退避时间、画面包计数和稳定错误分类。以下内容不会上传：

- 屏幕、光标图像、按键、剪贴板或文件内容；
- 设备 ID、访问密码、会话 ID、中继密钥、私钥或完整公钥身份；
- 未经长度限制和字段白名单验证的任意日志文本。

上传批次最大 48 KiB，使用当前 Windows 账户的 Ed25519 设备身份签名。服务端验证签名、64 KiB 上限、事件白名单、时间窗口和速率限制后才写入 SQLite。服务端只保存公钥的单向安装标识，不保存上传公钥和签名；批次与事件分别去重，数据最多保留 14 天。

主机端和控制端使用会话 ID 单向派生 32 位关联编号。服务器只能用它关联同一次连接两侧的脱敏事件，不能反推出原始会话 ID。

## 用户操作

在 Windows 客户端进入“设置”→“连接问题诊断”，开启“共享脱敏诊断”。DeskLink 每分钟尝试发送最近事件；网络失败后按最长 15 分钟退避，并在应用下次启动后继续补传。需要立即排查时可以点击“立即发送诊断”。

## 服务部署

服务以非特权用户监听 `127.0.0.1:3411`，公网只通过 `https://p2p.yxswy.com/desklink-diagnostics/` 的 Nginx 精确路径访问。部署必须来自干净 Git commit：

```powershell
python scripts/deploy-diagnostics-service.py
```

部署脚本创建不可变版本目录、原子切换 `current`、安装受限 systemd 服务、校验 Nginx 后再重载，并执行本机健康检查。

GitHub Actions 每半小时从外部生成临时 Ed25519 身份并提交一个签名探针批次，同时验证公网 HTTPS、签名校验、字段验证和服务端写入路径。

## 排查查询

通过 SSH 在服务器上查询最近 24 小时的 warning/error：

```sh
cd /opt/desklink-diagnostics/current
sudo -u desklink-diagnostics env DESKLINK_DIAGNOSTICS_DATABASE=/var/lib/desklink-diagnostics/diagnostics.sqlite \
  /usr/local/bin/bun run src/report.ts --hours 24 --limit 200
```

已知关联编号时增加 `--correlation <32位编号>`，可以按时间顺序查看同一次连接的主机端和控制端事件。

## 自动会话分析

服务会按关联编号自动识别以下模式：等待主机批准后未进入安全会话、四次及以上重连振荡、连接后没有完整视频帧、视频丢包率超过 10%，以及控制端或主机明确停止。分析不会读取屏幕、输入或访问密码，也不会产生新的公网接口。

手动分析最近 24 小时：

```sh
cd /opt/desklink-diagnostics/current
sudo -u desklink-diagnostics env DESKLINK_DIAGNOSTICS_DATABASE=/var/lib/desklink-diagnostics/diagnostics.sqlite \
  /opt/desklink-diagnostics/bin/bun run src/analyze.ts --hours 24
```

服务器每小时生成一次滚动 24 小时健康报告，文件为 `/var/lib/desklink-diagnostics/health-report.json`，权限仅限诊断服务账户和 root。任一错误会要求关注；没有错误但达到三个警告会话时同样标记 `requires_attention: true`。报告最多保留最近 100 个异常会话，不公开到网站。

本地已加载 SSH 密钥时，可以只读取基础设施状态和聚合计数，不下载异常会话明细：

```text
python scripts/audit-managed-diagnostics.py
```
