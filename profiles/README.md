# 领域配置边界

本目录说明配置归属，没有可加载的应用模板。Core 配置由可信调用方通过 Rust API 显式传入，Core 不读取部署配置文件。

领域配置定义及限制由[源码导航](../src/README.md)指向的所属模块维护；恢复流程配置变更的迁移要求见[领域维护](../docs/domain-maintenance.md)。认证、路由、连接地址、节点工作区、定时器和装配配置属于 Host。

维护规则见 [AGENTS](AGENTS.md)。
