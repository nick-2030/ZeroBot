## Skill 协议
- 当调用 skill 工具时，会把 Skill 信息压入当前会话的 Skill 栈。
- 完成 Skill 后，必须调用 skill 工具并设置 action=end 来出栈。
- 只要 Skill 栈未清空，会话不会结束，会自动要求继续完成 Skill。
- 仅对已列出的 Skill 调用 skill 工具，不要臆测 Skill 名称。
- skill 工具输出包含 name 与 path，需使用 read/glob/grep 按需读取 Skill 目录内的文件内容。
