import java.util.*;

public class BuddySystem {
    private final int blockSize;
    private final boolean[] memory;
    private final Map<Integer, Integer> allocations;

    public BuddySystem(int blockSize) {
        this.blockSize = blockSize;
        this.memory = new boolean[blockSize];
        Arrays.fill(this.memory, false);
        this.allocations = new HashMap<>();
    }

    // 尝试分配指定大小的内存块
    public int allocate(int size) {
        if (size <= 0 || size > blockSize || !isPowerOfTwo(size)) {
            throw new IllegalArgumentException("Invalid allocation size.");
        }
        int buddyIndex = findBuddy(size);
        if (buddyIndex == -1) {
            return -1;
        }
        for (int i = 0; i < size; i++) {
            memory[buddyIndex + i] = true;  // 标记为已使用
        }
        allocations.put(buddyIndex, size);
        return buddyIndex;
    }

    // 释放指定起始位置的内存块
    public void deallocate(int start) {
        if (!allocations.containsKey(start)) {
            throw new IllegalArgumentException("Invalid block to deallocate.");
        }
        int size = allocations.remove(start);
        for (int i = 0; i < size; i++) {
            memory[start + i] = false;  // 标记空闲
        }
        mergeFreeBlocks();
    }

    // 合并相邻的空闲块
    private void mergeFreeBlocks() {
        // 这里只是一个简单的示意，实际应该按照伙伴系统的规则进行合并
        // 实现伙伴系统的合并逻辑需要更复杂的算法
    }

    // 查找适合分配的伙伴块
    private int findBuddy(int size) {
        for (int i = 0; i <= blockSize - size; i++) {
            boolean isAvailable = true;
            for (int j = 0; j < size; j++) {
                if (memory[i + j]) {
                    isAvailable = false;  // 该块已被占用
                    break;
                }
            }
            if (isAvailable) {
                return i;
            }
        }
        return -1;
    }

    // 判断是否是2的幂次方
    private static boolean isPowerOfTwo(int n) {
        return (n & (n - 1)) == 0 && n != 0;
    }

    // 打印内存状态
    public void printMemoryStatus() {
        StringBuilder sb = new StringBuilder("Memory Status: ");
        for (boolean bit : memory) {
            sb.append(bit ? '1' : '0');
        }
        System.out.println(sb.toString());
    }

    public static void main(String[] args) {
        BuddySystem buddy = new BuddySystem(64);
        int allocatedAddress = buddy.allocate(8);
        System.out.println("Allocated at address: " + allocatedAddress);
        buddy.printMemoryStatus();
        buddy.deallocate(allocatedAddress);
        buddy.printMemoryStatus();
    }
}
