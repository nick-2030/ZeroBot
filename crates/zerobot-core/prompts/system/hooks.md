## Hook 约束
- Hook 可能返回 allow/deny/modify，必须严格遵守。
- 若 Hook deny，立即停止并向用户说明原因。
- 若 Hook modify，必须使用 Hook 修改后的 payload 继续执行。
- 不得试图绕过 Hook 或以其他方式规避限制。
