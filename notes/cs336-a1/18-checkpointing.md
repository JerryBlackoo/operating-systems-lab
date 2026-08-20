# 18 · 模型检查点（checkpointing）

讲义 §5.2 ｜ 1 分 ｜ adapters: `run_save_checkpoint` / `run_load_checkpoint` ｜ 测试: `uv run pytest -k test_checkpointing`

```text
Problem:
save_checkpoint / load_checkpoint — 序列化与恢复训练状态（两个纯函数）

输入:
save_checkpoint(model: nn.Module,
                optimizer: torch.optim.Optimizer,
                iteration: int,
                out: str | os.PathLike | BinaryIO | IO[bytes])
load_checkpoint(src: str | os.PathLike | BinaryIO | IO[bytes],
                model: nn.Module,
                optimizer: torch.optim.Optimizer) -> int

输出:
save: 无（写文件）
load: 返回保存时的 iteration；model/optimizer 被原地恢复

参数:
无

公式:
checkpoint = { "model":     model.state_dict(),
               "optimizer": optimizer.state_dict(),
               "iteration": iteration }

步骤:
save:
1. 组装上面的 dict
2. torch.save(obj, out)
load:
1. ckpt = torch.load(src)
2. model.load_state_dict(ckpt["model"])
3. optimizer.load_state_dict(ckpt["optimizer"])
4. return ckpt["iteration"]

需要查的 API:
- Module.state_dict() / Module.load_state_dict()
- Optimizer.state_dict() / Optimizer.load_state_dict()（含 m/v/t 状态）
- torch.save / torch.load（都支持路径或 file-like 对象）
- torch.load 的 map_location（跨设备恢复时）

边界情况:
- optimizer 状态必须保存: AdamW 的一阶/二阶矩和步数 t，
  否则恢复后动量清零、偏差修正重头来
- iteration 用于恢复学习率调度（schedule 是 t 的函数）
- out/src 既可能是路径也可能是打开的二进制文件对象，torch.save/load 都兼容
- RoPE buffer 用了 persistent=False 就不会进 state_dict（本来也不该存）
- 加载到不同 device 时用 map_location（可选增强）

测试:
- save -> load 后: 模型每个参数逐元素相等？optimizer.state 相等？
- 恢复后继续训练，loss 曲线是否与不中断的训练衔接？
- 返回的 iteration 是否等于保存时传入的值？
- uv run pytest -k test_checkpointing
```
