# 源码规范

继承[项目规范](../AGENTS.md)。只实现无状态业务计算；输入和输出不得包含可释放权限的权威对象。时间与事实由参数传入，不读取系统环境。

集中测试放在 [tests](../tests/README.md)。接口改动同步[调用契约](../docs/calling-contract.md)。
