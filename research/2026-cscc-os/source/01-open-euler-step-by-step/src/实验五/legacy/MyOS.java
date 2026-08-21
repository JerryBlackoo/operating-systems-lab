import java.io.*;
import java.util.*;
public class MyOS {
    /***************************************************
     * memory 操作系统挂载的内存
     * mmu mmu芯⽚实例
     * pcbs 进程表
     * reader Scanner读取⽤户输⼊
     * freeList 空闲⻚框表
     * pte pte表
     * ptecount pte计数器，协助⽣成虚拟地址
     **************************************************/
    private Memory memory;
    private MMU mmu;
    private ArrayList<MyPCB> pcbs;
    private Scanner reader;
    private ArrayList<Integer> freeList;
    private HashMap<Integer,Integer> pte;
    private int ptecount;
    /***********************************************************
     * @MethodName MyOS
     * @Description TODO 构造⽅法，初始化各成员
     * @Param []
     * @author Ayase
     *********************************************************/
    public MyOS(){
        this.memory = HardwareRepo.getMemory();
        this.pcbs = new ArrayList<>();
        this.reader = new Scanner(System.in);
//使⽤换⾏符作为输⼊分隔，即输⼊中可以包含空格
        reader.useDelimiter("\n");
        this.pte = new HashMap<>();
//初始化空闲⻚框表，添加的是⻚框索引
        this.freeList = new ArrayList<>();
        for(int i = 0 ; i < memory.getPageFrames().length ; i++){
            this.freeList.add(i);
        }
        this.ptecount = 0;
//将pte表传⼊mmu对象，让mmu可以查询pte表
        mmu = new MMU(pte);
    }
    /*************************************************************
     * @MethodName bash
     * @Description TODO 控制台⽅法，调⽤该⽅法后⽤户输⼊指令并作出对应处理
     * @Param []
     * @Return void
     * @author Ayase
     ************************************************************/
    public void bash(){
        exit:
        while(true){
            System.out.print("请输⼊指令：");
//将指令按空格分隔并存⼊字符创数组
            String[] str = reader.next().split(" ");
//判断输⼊指令类别并调⽤相应的处理⽅法
            switch(str[0]){
//新建进程
                case "np":{
                    newP();
                    break;
                }
//释放指定进程号进程，使⽤范例：“free 0
                case "free":{
                    freeP(Integer.parseInt(str[1]));
                    break;
                }
//打印指定进程号进程数据，使⽤范例：”pp 0“
                case "pp":{
                    printP(Integer.parseInt(str[1]));
                    break;
                }
//检查指定进程号进程数据是否与内存⼀致，使⽤范例：”cp 0“
                case "cp":{
                    checkP(Integer.parseInt(str[1]));
                    break;
                }
//更改tlb替换策略，使⽤范例：”cf random“、”cf lru“
                case "cf":{
                    changeFlag(str[1]);
                    break;
                }
//打印进程情况
                case "ps":{
                    processStatus();
                    break;
                }
//打印当前tlb替换策略
                case "tp":{
                    mmu.getFlag();
                    break;
                }
//退出系统
                case "exit":break exit;
                default:
                    System.out.println("没有这个指令！");break;
            }
            System.out.println();
            System.out.println("-----------------------------------------");
        }
    }
    /***********************************************************
     * @MethodName newP
     * @Description TODO 新建进程⽅法
     * @Param []
     * @Return void
     * @author Ayase
     *********************************************************/
    public void newP(){
        int dataSize = (int)(Math.random()*HardwareRepo.SIZE_MEMORY/2)+1;
//下⼀⾏代码⽤于测试需要使⽤固定⻚框数量的进程的情况
//int dataSize = 5;
//判断是否有⾜够的内存供分配
        if(dataSize > freeList.size()){
            System.out.println("内存不⾜！还需空间:" + (dataSize - freeList.size()));
            return;
        }
//新建对应⻓度的进程
        MyPCB pcb = new MyPCB(dataSize);
        System.out.println("新建进程:" + pcb.getId() + " ⻓度:" + dataSize);
        for(int i = 0 ; i < dataSize ; i++){
//空闲⻚框索引出队
            int index = freeList.get(0);
            freeList.remove(0);
//将进程数据写⼊内存
            char[] data = new char[PageFrame.PAGE_SIZE];
            for(int j = 0 ; j < PageFrame.PAGE_SIZE ; j++){
                data[j] = pcb.getExecuteData()[i*PageFrame.PAGE_SIZE+j];
            }
            memory.setPageFrame(index,data);
//新增pte
            pte.put(ptecount,index);
//⽣成虚拟地址并向pcb添加虚拟地址
            for(int j = 0;j < PageFrame.PAGE_SIZE ; j++){
                pcb.getAddrs().add(ptecount*PageFrame.PAGE_SIZE+j);
            }
            ptecount++;
        }
//进程表添加进程
        pcbs.add(pcb);
    }
    /***********************************************************
     * @MethodName freeP
     * @Description TODO 释放指定进程号的进程
     * @Param [n]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void freeP(int n){
//遍历进程表查找进程
        for(int i = 0 ; i < pcbs.size();i++){
//若找到指定进程号的进程
            if(pcbs.get(i).getId() == n){
                MyPCB p = pcbs.get(i);
                for(int j = 0 ; j < p.getAddrs().size()/PageFrame.PAGE_SIZE;j++){
//将虚拟地址转成物理地址
                    int addr = mmu.toRealAddr(p.getAddrs().get(j*PageFrame.PAGE_SIZE));
//指定物理地址的⻚框添加到空闲⻚框表
                    freeList.add(addr/PageFrame.PAGE_SIZE);
//清理内存数据
                    memory.clear(addr/PageFrame.PAGE_SIZE);
//清除指定⻚号的pte
                    pte.remove(p.getAddrs().get(j*PageFrame.PAGE_SIZE)/PageFrame.PAGE_SIZE);
                }
//清除pcb
                pcbs.remove(i);
//打印转换物理地址时tlb命中率
                mmu.printHitRate();
                System.out.println("已释放进程" + n);
                return;
            }
        }
        System.out.println("找不到指定的进程！");
    }
/***********************************************************
 * @MethodName printP
 * @Description TODO 打印指定进程号的进程对应内存的数据
 * @Param [n]
 * @Return void
 * @author Ayase
 *********************************************************/
public void printP(int n){
//遍历查找进程
    for(int i = 0 ; i < pcbs.size();i++){
//若找到
        if(pcbs.get(i).getId() == n){
            MyPCB p = pcbs.get(i);
//转换为物理地址后访存打印数据
            for(int j = 0 ; j < p.getAddrs().size()/PageFrame.PAGE_SIZE;j++){
                int addr = mmu.toRealAddr(p.getAddrs().get(j*PageFrame.PAGE_SIZE));
                memory.printData(addr/PageFrame.PAGE_SIZE);
            }
//打印转换物理地址tlb命中率
            mmu.printHitRate();
            System.out.println();
            return;
        }
    }
    System.out.println("找不到指定的进程！");
}
    /***********************************************************
     * @MethodName checkP
     * @Description TODO 通过检查数据是否⼀致，以检查地址转换是否正确
     * @Param [n]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void checkP(int n){
//遍历查找进程
        for(int i = 0 ; i < pcbs.size();i++){
//若找到
            if(pcbs.get(i).getId() == n){
                MyPCB p = pcbs.get(i);
//遍历所有记录的虚拟地址，按位检查数据
                for(int j = 0 ; j < p.getAddrs().size();j++){
//从pcb中获取虚拟地址经过mmu转换获得物理地址，然后通过物理地址获取数据
//若数据不⼀致则通知⽤户数据错误并返回
                    if(p.getExecuteData()[j] != memory.getData(mmu.toRealAddr(p.getAddrs().get(j)))){
                        System.out.println("数据不⼀致！");
                        mmu.printHitRate();
                        return;
                    }
                }
//所有数据检查⽆误通知⽤户数据⼀致
                System.out.println("数据⼀致！");
//打印地址转换中tlb命中率
                mmu.printHitRate();
                System.out.println();
                return;
            }
        }
        System.out.println("找不到指定的进程！");
    }
/***********************************************************
 * @MethodName changeFlag
 * @Description TODO 更换tlb替换策略
 * @Param [policy]
 * @Return void
 * @author Ayase
 *********************************************************/
public void changeFlag(String policy){
    switch (policy){
        case "random":{
            mmu.setFlag(MMU.RANDOM_REMOVE);
            System.out.println("已修改tlb替换策略:random");
            break;
        }
        case "lru":{
            mmu.setFlag(MMU.LRU_REMOVE);
            System.out.println("已修改tlb替换策略:lru");
            break;
        }
        default:
            System.out.println("没有这个tlb替换策略！");
    }
}
    /***********************************************************
     * @MethodName processStatus
     * @Description TODO 打印当前进程状态
     * @Param []
     * @Return void
     * @author Ayase
     *********************************************************/
    public void processStatus(){
//若当前进程表⻓度为0则通知⽤户没有进程并返回
        if(pcbs.size() == 0){
            System.out.println("当前没有进程！");
            return;
        }
        StringBuffer str = new StringBuffer();
        int count = 0;
        int spaceCount = 0;
//遍历所有进程并将信息添加到输出字符串中
        for(MyPCB pcb:pcbs){
            spaceCount += pcb.getAddrs().size()/PageFrame.PAGE_SIZE;
            str.append("{count = " + count++ + " , pid = " + pcb.getId() + " , useSpace = " +
                    pcb.getAddrs().size()/PageFrame.PAGE_SIZE + " }\n");
        }
//打印进程信息
        System.out.print(str);
//打印统计信息
        System.out.println("总计进程数:" + count + " 共占⽤⻚框:" + spaceCount);
    }
}
