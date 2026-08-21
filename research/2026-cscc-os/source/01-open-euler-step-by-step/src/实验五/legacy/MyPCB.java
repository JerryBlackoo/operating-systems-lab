import java.util.ArrayList;
/**
 * @author Ayase
 * @date 2021/5/23-10:23
 */
public class MyPCB {
    /******************************************************************
     * idcount 静态进程号计数，⽤于分配进程号
     * id 进程id
     * addrs 进程数据的虚拟地址
     * executeData 为程序数据，本实验中⽤于验证答案，在现实情况中PCB不保存这些数据
     *****************************************************************/
    private static int idcount = 0;
    private int id;
    private ArrayList<Integer> addrs;
    private char[] executeData;
    /*******************************************************************
     * @MethodName MyPCB
     * @Description TODO 构造⽅法，传⼊的⻓度表⽰进程数据占⽤多少⻚框。随机填⼊数据
     * @Param [n]
     * @author Ayase
     *******************************************************************/
    public MyPCB(int n){
        this.id = idcount++;
        executeData = new char[n*PageFrame.PAGE_SIZE];
        addrs = new ArrayList<>();
//随机⽣成数据
        for (int i = 0 ; i < n*PageFrame.PAGE_SIZE; i++){
            char c = (char)(Math.random()*26+97);
            executeData[i] = c;
        }
    }
    /***********************************************************
     * @MethodName getId
     * @Description TODO 获取进程id
     * @Param []
     * @Return int
     * @author Ayase
     *********************************************************/
    public int getId() {
        return id;
    }
    /***********************************************************
     * @MethodName getAddrs
     * @Description TODO 获取进程地址表
     * @Param []
     * @Return java.util.ArrayList<java.lang.Integer>
     * @author Ayase
     *********************************************************/
    public ArrayList<Integer> getAddrs(){
        return addrs;
    }
    /***********************************************************
     * @MethodName getExecuteData
     * @Description TODO 获取进程数据
     * @Param []
     * @Return char[]
     * @author Ayase
     *********************************************************/
    public char[] getExecuteData() {
        return executeData;
    }
}
