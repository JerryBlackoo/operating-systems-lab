using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Diagnostics;
using Microsoft.Win32.SafeHandles;
public static class PfTest3 {
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetProcessWorkingSetSize(IntPtr hp, IntPtr mn, IntPtr mx);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr CreateFileMapping(SafeFileHandle f, IntPtr a, uint p, uint dm, uint sm, string n);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr MapViewOfFile(IntPtr h, uint a, uint o, uint n, UIntPtr s);
    static void touch(IntPtr baseAddr, long size) {
        long b = baseAddr.ToInt64();
        for (long i=0;i<size;i+=4096) Marshal.ReadByte(new IntPtr(b+i));
    }
    public static void Run() {
        string path = @"D:\proj59\results\pf_test.bin";
        var fs = new FileStream(path, FileMode.Open, FileAccess.ReadWrite, FileShare.ReadWrite);
        IntPtr fm = CreateFileMapping(fs.SafeFileHandle, IntPtr.Zero, 0x04, 0, 128*1024*1024, null);
        IntPtr baseAddr = MapViewOfFile(fm, 0x0006, 0, 0, UIntPtr.Zero);
        touch(baseAddr, 128L*1024*1024);          // ??????
        var sw = new Stopwatch();
        // ???trim ?????
        SetProcessWorkingSetSize(GetCurrentProcess(), (IntPtr)(-1), (IntPtr)(-1));
        sw.Restart(); touch(baseAddr, 128L*1024*1024); sw.Stop();
        long t0 = sw.ElapsedMilliseconds;
        // ?????? 100ms ??
        SetProcessWorkingSetSize(GetCurrentProcess(), (IntPtr)(-1), (IntPtr)(-1));
        var t = new Thread(() => touch(baseAddr, 128L*1024*1024));
        t.IsBackground = true; t.Start();
        Thread.Sleep(100);
        sw.Restart(); touch(baseAddr, 128L*1024*1024); sw.Stop();
        long t1 = sw.ElapsedMilliseconds;
        t.Join();
        Console.WriteLine("main-touch time: no-prefetch=" + t0 + "ms  after 100ms prefetch-thread=" + t1 + "ms");
    }
}
