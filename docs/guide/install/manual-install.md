# NodeGet 手动二进制部署指南

对于想要折腾一遍手动安装过程的用户，这里提供一些简要的安装参考意见。

NodeGet 的 [GitHub Releases](https://github.com/NodeSeekDev/NodeGet/releases) 提供了可直接运行的静态编译二进制程序。

你可以下载和你系统相匹配的版本，根据 [参数和配置](/guide/config/) 一章提供的配置模板编写配置文件，然后启动程序指定配置文件：

```shell
./nodeget serve -c path-to-config.toml
```

屏幕打印出的 Token 和密码都只显示一次，请及时保存。

为了保持程序异常退出和开机后能够自动重启，需要守护进程。可以用系统自带的 `systemd` 等进程管理器，也可以用 `supervisor` 等方案。

如果你遇到了问题，日志是最好的朋友。

## 验证 Agent 监控功能

在将 Agent 连接到 Server 之前，建议先使用 `--dry-run` 模式验证 Agent 的本地监控功能是否正常：

```shell
./nodeget-agent -c config.toml --dry-run
```

该命令会：

- 执行一次静态监控数据采集（CPU、系统信息、GPU 硬件信息）
- 执行一次动态监控数据采集（CPU 使用率、内存、磁盘、网络、GPU 状态等）
- 打印完整的监控数据到终端
- 验证磁盘和网络接口的过滤逻辑
- 进程执行完成后自动退出

通过观察输出，可以确认：

- 系统信息是否正确识别（CPU 型号、架构、虚拟化状态等）
- GPU 是否正常检测（NVIDIA GPU 的显存、温度、利用率等）
- 磁盘和网络统计是否准确
- 磁盘挂载点和网络接口的过滤规则是否生效（排除了 Docker、Loopback 等虚拟接口）

如果输出结果符合预期，说明 Agent 的监控模块工作正常，可以放心连接到 Server。