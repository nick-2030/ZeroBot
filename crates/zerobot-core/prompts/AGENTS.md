# zerobot-core/prompts

## OVERVIEW
提示词模块目录：system 负责全局行为约束，modes 负责运行模式差异（execute/review/plan）。

## STRUCTURE
```text
.
├── system/
│   ├── identity.md
│   ├── behavior.md
│   ├── security.md
│   ├── tools.md
│   ├── tools_parallel.md
│   ├── task_mgmt.md
│   ├── hooks.md
│   ├── skills.md
│   ├── style.md
│   └── output.md
└── modes/
    ├── execute.md
    ├── review.md
    └── plan.md
```

## WHERE TO LOOK
| Need | File | Key Constraint |
|------|------|----------------|
| 行为边界 | `system/behavior.md` | 不越权、不擅自扩展、不提交/推送 |
| 工具策略 | `system/tools.md` | 专用工具优先、拒绝后不重复、read 先于写 |
| 并行策略 | `system/tools_parallel.md` | 独立并行、依赖顺序执行 |
| Hook 协议 | `system/hooks.md` | modify 时必须使用修改后 payload |
| Skill 生命周期 | `system/skills.md` | Skill 完成必须 `skill action=end` |
| 计划模式 | `modes/plan.md` | 严格只读，禁止任何文件改动 |

## CONVENTIONS
- 该目录只放提示词文本，不放运行时代码。
- 修改约束类文案时，先检查 `docs/prompt.md` 是否需要同步说明。
- Mode 文档必须显式声明允许/禁止行为，避免与 system 规则冲突。

## ANTI-PATTERNS
- 在 mode 文档里重复整段 system 规则（会造成漂移）。
- 修改 `plan.md` 但破坏“只读模式”硬约束。
- 新增规则但无可执行表达（模糊语句、不可验证）。
