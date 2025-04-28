# 希冀 实验七：文件管理

> Source: https://blog.csdn.net/m0_74172897/article/details/144415514

## Tree FS ops to implement / complete
| Op | Role |
|----|------|
| `mkdir` | create directory child under `recent` |
| `create` | create file node |
| `read` / `write` | transfer data; **two sites** (`child` and `next`) often both need edits |
| `del` | delete file |
| `rm` | delete directory |
| `open` / `close` | thinking questions / related helpers |

## Pitfalls
- Duplicate name checks for dirs vs files (`isdir` flag)
- Sibling list via `prev/next`; children via `child`
- Platform notes: `read()` has two `printf` paths; `write()` two `scanf` paths—update both

## Takeaway
In-memory FS = tree of nodes with type bit + linked siblings; correctness is pointer surgery + name uniqueness.
