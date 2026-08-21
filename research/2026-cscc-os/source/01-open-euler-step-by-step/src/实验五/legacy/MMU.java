import java.util.HashMap;

public class MMU {
    /******************************************************
     * RANDOM_REMOVE 随机替换策略标志
     * LRU_REMOVE LRU替换策略标志
     * NUM_TLB TLB表⻓度
     * TLBList TLB表
     * pte pte表指向操作系统中的pte表
     * hitcount tlb命中情况统计，0位为命中，1位为未命中
     * flag 当前使⽤的替换策略标志
     *****************************************************/
    public static final int RANDOM_REMOVE = 0;
    public static final int LRU_REMOVE = 1;
    public static final int NUM_TLB = 10;
    private TLB[] TLBList;
    private HashMap<Integer,Integer> pte;
    private int[] hitcount;
    private int flag;
    private int[] bitmap = new int[NUM_TLB];

    /**************************************************
     * 私有静态内部类，外部类不能实例化这个类
     * TLB类，记录tlb项信息
     *************************************************/
    private static class TLB{
        /********************************************
         * pageId ⻚号
         * pageFrameId ⻚框号
         *******************************************/
        private int pageId;
        private int pageFrameId;
        /***********************************************************
         * @MethodName TLB
         * @Description TODO 构造⽅法，输⼊⻚号和⻚框号新建tlb项
         * @Param [pageId, pageFrameId]
         * @author Ayase
         *********************************************************/
        TLB(int pageId,int pageFrameId){
            this.pageId = pageId;
            this.pageFrameId = pageFrameId;
        }
        /***********************************************************
         * @MethodName toString
         * @Description TODO toString⽅法，返回tlb项信息字符串
         * @Param []
         * @Return java.lang.String
         * @author Ayase
         *********************************************************/
        @Override
        public String toString() {
            return "TLB{" +
                    pageId +
                    ", " + pageFrameId +
                    '}';
        }
    }
/***********************************************************
 * @MethodName MMU
 * @Description TODO 构造⽅法，初始化各成员
 * @Param [pte]
 * @author Ayase
 *********************************************************/
public MMU(HashMap<Integer,Integer> pte){
    this.TLBList = new TLB[NUM_TLB];
    this.pte = pte;
    this.hitcount = new int[2];
    this.flag = RANDOM_REMOVE;
}
    /***********************************************************
     * @MethodName toRealAddr
     * @Description TODO 将虚拟内存转化为物理内存
     * @Param [virtualAddr]
     * @Return int
     * @author Ayase
     *********************************************************/
    public int toRealAddr(int virtualAddr) {
        int pageId = virtualAddr / 8; // 例如，每页大小为 4KB
        int offset = virtualAddr % 8;

        TLB tlbEntry = findTLB(pageId);
        if (tlbEntry != null) {
            hitcount[0]++; // 命中
            return tlbEntry.pageFrameId * 8+ offset;
        } else {
            hitcount[1]++; // 未命中
            int pageFrameId = pte.getOrDefault(pageId, -1);
            if (pageFrameId == -1) {
                throw new RuntimeException("页号未找到！");
            }
            newTLB(pageId, pageFrameId, flag);
            return pageFrameId * 8+ offset;
        }
    }
    /***********************************************************
     * @MethodName findTLB
     * @Description TODO 查找tlb项，若未找到则返回Null
     * @Param [pageId]
     * @Return entity.MMU.TLB
     * @author Ayase
     *********************************************************/
    private TLB findTLB(int pageId){
        for(int i = 0 ; i < TLBList.length ; i++){
            if(TLBList[i] == null){
                break;
            }
            if(TLBList[i].pageId == pageId){
                bitmap[i]++;
                return TLBList[i];
            }
        }
        return null;
    }
    /***********************************************************
     * @MethodName newTLB
     * @Description TODO 新建TLB项，若表已满则按照替换策略替换tlb项
     * @Param [pageId, pageFrameId,flag]
     * @Return void
     * @author Ayase
     *********************************************************/
    private void newTLB(int pageId,int pageFrameId,int flag){
        if(isTLBFull()){
            switch (flag){
                case RANDOM_REMOVE:{
                    int index = randomRemove();
                    TLBList[index] = new TLB(pageId,pageFrameId);
                    bitmap[index] = 0;
                    break;
                }
                case LRU_REMOVE:{
                    int index = lruRemove();
                    TLBList[index] = new TLB(pageId,pageFrameId);
                    bitmap[index] = 0;
                    break;
                }
                default:throw new IllegalArgumentException("⾮法参数!");
            }
        }
        else {
//未满则添加到空闲位
            for(int i = 0 ; i < TLBList.length ; i++){
                if(TLBList[i] == null){
                    TLBList[i] = new TLB(pageId,pageFrameId);
                    return;
                }
            }
        }
    }
    /***********************************************************
     * @MethodName lruRemove
     * @Description TODO lru替换策略
     * @Param []
     * @Return int
     * @author Ayase
     *********************************************************/
    private int lruRemove() {
        int minIndex = 0;
        int minValue = bitmap[0];

        for (int i = 1; i < bitmap.length; i++) {
            if (bitmap[i] < minValue) {
                minValue = bitmap[i];
                minIndex = i;
            }
        }
        return minIndex;
    }

    /***********************************************************
     * @MethodName randomRemove
     * @Description TODO 随机替换
     * @Param []
     * @Return int
     * @author Ayase
     *********************************************************/
    private int randomRemove() {
        return (int) (Math.random() * NUM_TLB);
    }
    /**********************************************************
     * @MethodName isTLBFull
     * @Description TODO 判断tlb表是否满
     * @Param []
     * @Return boolean
     * @author Ayase
     *********************************************************/
    private boolean isTLBFull(){
        for(int i = 0 ; i < TLBList.length ; i++){
            if(TLBList[i] == null){
                return false;
            }
        }
        return true;
    }
    /***********************************************************
     * @MethodName printHitRate
     * @Description TODO 打印命中率并清空计数器
     * @Param []
     * @Return void
     * @author Ayase
     *********************************************************/
    public void printHitRate(){
        float hitRate = (float) hitcount[0]/(float) (hitcount[0]+hitcount[1]);
        System.out.println("HitRate:" + (int)(hitRate*100) + "%");
        printTLBList();
        hitcount[0] = 0;
        hitcount[1] = 0;
    }
    /***********************************************************
     * @MethodName printTLBList
     * @Description TODO 打印tlb表
     * @Param []
     * @Return void
     * @author Ayase
     *********************************************************/
    public void printTLBList(){
        StringBuffer str = new StringBuffer();
        int i= 0;
        for(TLB tlb:TLBList){
            if(tlb == null){
                break;
            }
            str.append(i++ +"|" + tlb.toString() +" ");
        }
        System.out.println(str);
    }
    /***********************************************************
     * @MethodName setFlag
     * @Description TODO 设置tlb替换策略
     * @Param [flag]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void setFlag(int flag){
        if(flag != RANDOM_REMOVE && flag != LRU_REMOVE){
            throw new IllegalArgumentException("⾮法tlb替换策略参数！");
        }
        this.flag = flag;
    }
    /***********************************************************
     * @MethodName getFlag
     * @Description TODO 获取当前tlb替换策略
     * @Param []
     * @Return void
     * @author Ayase
     *********************************************************/
    public void getFlag(){
        switch (flag){
            case RANDOM_REMOVE:
                System.out.println("当前tlb替换策略是random");
                break;
            case LRU_REMOVE:
                System.out.println("当前tlb替换策略是lru");
                break;
            default:throw new RuntimeException("tlb替换策略异常！");
        }
    }
}
