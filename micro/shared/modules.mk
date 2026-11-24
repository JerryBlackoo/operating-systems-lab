# 模块目录表：每个模块声明“编译哪些文件”，以及“还依赖哪些实现”。
#
#   MODEL_DIR_<id>  模块目录（相对 micro/）
#   MODEL_FILES_<id>该模块自己的源文件，必须包含 demo.c
#   MODEL_SUPPORT_<id> 链接该模块时额外需要的支撑实现（可传递）
#
# 支撑链按依赖方向从底向上：
#   OBJECT_SUPPORT → MEMORY_SUPPORT → SCHED_SUPPORT → MODEL_SUPPORT_ipc
# 内核模块由 micro/Makefile 选择后直接编译；STANDALONE_MODELS 里的模块
# 另外用 demo.mk 各自链接单模块镜像。

STANDALONE_MODELS := syscall abi init libs apps pm sm vm vfs block ext4 net nvme
BOOT_MODELS := capability clock interrupt objects memory sched ipc $(STANDALONE_MODELS)

MODEL_DIR_capability := kernel/capability
MODEL_FILES_capability := capability.c cap_arch_riscv.c cap_selftest.c demo.c
MODEL_DIR_clock := kernel/clock
MODEL_FILES_clock := timebase.c timer.c event.c demo.c
MODEL_DIR_interrupt := kernel/interrupt
MODEL_FILES_interrupt := irq.c demo.c
MODEL_DIR_objects := kernel/objects
MODEL_FILES_objects := object.c demo.c
MODEL_DIR_memory := kernel/memory
MODEL_FILES_memory := frame.c as.c demo.c
MODEL_DIR_sched := kernel/sched
MODEL_FILES_sched := sched.c switch_rv64.S demo.c
MODEL_DIR_ipc := kernel/ipc
MODEL_FILES_ipc := ipc.c demo.c

OBJECT_SUPPORT := kernel/objects/object.c
MEMORY_SUPPORT := $(OBJECT_SUPPORT) kernel/memory/frame.c kernel/memory/as.c
SCHED_SUPPORT := $(MEMORY_SUPPORT) kernel/sched/sched.c kernel/sched/switch_rv64.S
CLOCK_SUPPORT := kernel/clock/timebase.c kernel/clock/timer.c kernel/clock/event.c
MODEL_SUPPORT_memory := $(OBJECT_SUPPORT)
MODEL_SUPPORT_sched := $(SCHED_SUPPORT) $(CLOCK_SUPPORT) kernel/ipc/ipc.c
MODEL_SUPPORT_ipc := $(SCHED_SUPPORT) $(CLOCK_SUPPORT)
# capability demo 把能力表接到真实对象链路上：endpoint / frame / AS / 调度 / syscall。
MODEL_SUPPORT_capability := $(SCHED_SUPPORT) $(CLOCK_SUPPORT) kernel/ipc/ipc.c kernel/syscall/syscall.c
# interrupt demo 用真实线程与 endpoint 演示“中断即 IPC 通知”；
# irq.c 本体仍然只依赖 irq_port，不依赖 sched/ipc。
MODEL_SUPPORT_interrupt := $(SCHED_SUPPORT) $(CLOCK_SUPPORT) kernel/ipc/ipc.c
# vm 的策略模型只依赖 vm_port；demo 用 kernel/memory 实例化该端口。
MODEL_SUPPORT_vm := $(MEMORY_SUPPORT)
# pm 的策略模型只依赖 pm_port；demo 用 memory/sched/capability 实例化三份资源。
MODEL_SUPPORT_pm := $(MODEL_SUPPORT_sched) kernel/capability/capability.c kernel/capability/cap_arch_riscv.c

# syscall demo 把分发器接到真实的能力、IPC、页表和调度实现。
MODEL_SUPPORT_syscall := $(SCHED_SUPPORT) $(CLOCK_SUPPORT) kernel/ipc/ipc.c \
                         kernel/capability/capability.c kernel/capability/cap_arch_riscv.c
MODEL_DIR_syscall := kernel/syscall
MODEL_FILES_syscall := syscall.c demo.c
MODEL_DIR_abi := shared/abi
MODEL_FILES_abi := demo.c
MODEL_DIR_init := userland/init
MODEL_FILES_init := init.c demo.c
# init 的启动端口接到能力派生、进程管理和服务注册三个真实模型。
MODEL_SUPPORT_init := kernel/capability/capability.c kernel/capability/cap_arch_riscv.c \
                      userland/servers/pm/pm.c userland/servers/sm/sm.c
MODEL_DIR_libs := userland/libs
MODEL_FILES_libs := runtime.c demo.c
MODEL_DIR_apps := userland/apps
MODEL_FILES_apps := echo.c demo.c
MODEL_DIR_pm := userland/servers/pm
MODEL_FILES_pm := pm.c demo.c
MODEL_DIR_sm := userland/servers/sm
MODEL_FILES_sm := sm.c demo.c
MODEL_DIR_vm := userland/servers/vm
MODEL_FILES_vm := vm.c demo.c
MODEL_DIR_vfs := userland/servers/vfs
MODEL_FILES_vfs := vfs.c demo.c
MODEL_DIR_block := userland/servers/block
MODEL_FILES_block := block.c demo.c
MODEL_DIR_ext4 := userland/servers/fs/ext4
MODEL_FILES_ext4 := ext4.c demo.c
MODEL_DIR_net := userland/servers/net
MODEL_FILES_net := udp.c demo.c
MODEL_DIR_nvme := userland/servers/drivers/storage/nvme
MODEL_FILES_nvme := nvme.c demo.c
