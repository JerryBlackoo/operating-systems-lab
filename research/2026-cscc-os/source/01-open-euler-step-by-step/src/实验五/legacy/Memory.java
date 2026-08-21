public class Memory {
    /**************************************
     * pageFrames ⻚框表，内存以⻚框形式维护
     *************************************/
    private PageFrame[] pageFrames;
    /***********************************************************
     * @MethodName Memory
     * @Description TODO 构造⽅法，传⼊指定的内存⼤⼩构造内存对象
     * @Param [size]
     * @author Ayase
     *********************************************************/
    public Memory(int size){
        pageFrames = new PageFrame[size];
        for(int i = 0; i < size ; i++){
            pageFrames[i] = new PageFrame();
        }
    }
    /***********************************************************
     * @MethodName getPageFrames
     * @Description TODO 获取⻚框表，深拷⻉防⽌修改
     * @Param []
     * @Return entity.PageFrame[]
     * @author Ayase
     *********************************************************/
    public PageFrame[] getPageFrames() {
        PageFrame[] temp = new PageFrame[pageFrames.length];
        for(int i = 0 ; i < temp.length ; i++){
            //复制⻚框数据
            temp[i] = new PageFrame(pageFrames[i]);
        }
        return temp;
    }
    /***********************************************************
     * @MethodName setPageFrame
     * @Description TODO 传⼊指定index和data，修改对应⻚框的数据
     * @Param [pageFrames]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void setPageFrame(int n,char[] data) {
        if (n >this.pageFrames.length){
            throw new IndexOutOfBoundsException("索引n⼤于内存⼤⼩" + n);
        }
        this.pageFrames[n].setData(data);
    }
    /***********************************************************
     * @MethodName clear
     * @Description TODO 清除指定索引⻚框
     * @Param [n]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void clear(int n){
        pageFrames[n].clear();
    }
    public void printData(){
        for (int i = 0 ; i < pageFrames.length ; i++){
            pageFrames[i].printData();
        }
        System.out.println();
    }
    /***********************************************************
     * @MethodName printData
     * @Description TODO 打印指定索引⻚框数据
     * @Param [n]
     * @Return void
     * @author Ayase
     *********************************************************/
    public void printData(int n){
        System.out.print(n);
        pageFrames[n].printData();
    }
    /***********************************************************
     * @MethodName toString
     * @Description TODO 返回所有内存数据字符串
     * @Param []
     * @Return java.lang.String
     * @author Ayase
     *********************************************************/
    @Override
    public String toString(){
        StringBuffer str = new StringBuffer();
        for(int i = 0 ; i < pageFrames.length ; i++){str.append(pageFrames[i].toString());
        }
        return str.toString();
    }
    /***********************************************************
     * @MethodName getData
     * @Description TODO 传⼊指定物理地址获取指定⻚框数据
     * @Param [realAddr]
     * @Return char
     * @author Ayase
     *********************************************************/
    public char getData(int realAddr){
        return pageFrames[realAddr/PageFrame.PAGE_SIZE].getData()[realAddr%PageFrame.PAGE_SIZE];
    }
}
