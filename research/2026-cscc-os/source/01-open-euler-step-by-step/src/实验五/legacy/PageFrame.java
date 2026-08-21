public class PageFrame {
    /************************************************************
     * PAGE_SIZE ⻚框⼤⼩
     * data ⻚框内数据
     ***********************************************************/
    public static final int PAGE_SIZE = 8;
    private char[] data;
    /***********************************************************
     * @MethodName PageFrame
     * @Description TODO ⽆参构造⽅法
     * @Param []
     * @author Ayase
     *********************************************************/
    PageFrame(){
        data = new char[PAGE_SIZE];
    }
    /***********************************************************
     * @MethodName PageFrame
     * @Description TODO 构造⽅法，复制⻚框
     * @Param [pf]
     * @author Ayase
     *********************************************************/
    PageFrame(PageFrame pf){
        this();
        setData(pf.getData());
    }
    /***********************************************************
     * @MethodName getData
     * @Description TODO 返回data，深拷⻉防恶意修改
     * @Param []
     * @Return char[]
     * @author Ayase
     *********************************************************/
    public char[] getData(){
        char[] temp = new char[PAGE_SIZE];
        for(int i = 0 ; i < PAGE_SIZE ; i++){
            temp[i] = data[i];
        }
        return temp;
    }
    /***********************************************************
     * @MethodName setData
     * @Description TODO 修改data
     * @Param [data]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void setData(char[] data) {
        this.data = data;
    }
    /***********************************************************
     * @MethodName clear
     * @Description TODO 清空⻚框内数据
     * @Param []
     * @Return void
     * @author Ayase
     *********************************************************/
    public void clear(){
        this.data = new char[PAGE_SIZE];
    }
/***********************************************************
 * @MethodName printData
 * @Description TODO 打印⻚框内数据
 * @Param []
 * @Return void
 * @author Ayase
 *********************************************************/
public void printData(){
    System.out.println("|" + new String(data));
}
    /***********************************************************
     * @MethodName toString
     * @Description TODO toString⽅法返回字符串
     * @Param []
     * @Return java.lang.String
     * @author Ayase
     *********************************************************/
    @Override
    public String toString(){
        return new String(data);
    }
}
