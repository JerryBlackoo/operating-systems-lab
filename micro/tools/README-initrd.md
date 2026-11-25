# initrd 格式 MIRD v1

```text
"MIRD" (4) | version u32 (1) | count u32 | members...
member = name_len u32 | data_len u32 | name | data | pad(member 整体 to 4)
```

打包：

```sh
python3 tools/mkinitrd.py build/initrd.img build/user/init.elf
```

内核用 `loader_find_initrd(..., "init", ...)` 按 **stem** 匹配（`init.elf` → `init`）。