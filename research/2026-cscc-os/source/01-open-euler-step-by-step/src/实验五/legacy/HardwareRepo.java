/**
 * @author Ayase
 * @date 2021/5/23-10:34
 */
public class HardwareRepo {
    /*********************************************
     * SIZE_MEMORY 内存⼤⼩
     * memory 静态内存
     ********************************************/
    public static final int SIZE_MEMORY = 64;
    private static Memory memory;
    /***********************************************************
     * @MethodName getMemory
     * @Description TODO 获取内存
     * @Param []
     * @Return entity.Memory
     * @author Ayase
     *********************************************************/
    public static Memory getMemory() {
//若为空则初始化内存
        if(memory == null){
            memory = new Memory(SIZE_MEMORY);
        }
        return memory;
    }
}

