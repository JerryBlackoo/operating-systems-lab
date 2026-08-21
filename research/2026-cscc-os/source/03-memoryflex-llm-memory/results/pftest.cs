using System;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class PfTest {
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool SetProcessWorkingSetSize(IntPtr hp, IntPtr mn, IntPtr mx);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool PrefetchVirtualMemory(IntPtr hp, UIntPtr n, WIN32_MEMORY_RANGE_ENTRY[] e, uint f);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetProcessMemoryInfo(IntPtr hp, out PMC pmc, uint cb);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr CreateFileMapping(SafeFileHandle f, IntPtr a, uint p, uint dm, uint sm, string n);
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr MapViewOfFile(IntPtr h, uint a, uint o, uint n, UIntPtr s);
    [StructLayout(LayoutKind.Sequential)] public struct WIN32_MEMORY_RANGE_ENTRY { public IntPtr VirtualAddress; public UIntPtr NumberOfBytes; }
    [StructLayout(LayoutKind.Sequential)] public struct PMC { public uint cb,PageFaultCount; public UIntPtr PeakWS,WS,QP,QP2,QN,QN2,PF,PF2; }
    static ulong faults() { PMC p=new PMC(); p.cb=(uint)Marshal.SizeOf(p); GetProcessMemoryInfo(GetCurrentProcess(), out p, p.cb); return p.PageFaultCount; }
    public static void Run() {
        // 128MB ????
        string path = @"D:\proj59\results\pf_test.bin";
        File.WriteAllBytes(path, new byte[1]);
        var fs = new FileStream(path, FileMode.Open, FileAccess.ReadWrite, FileShare.ReadWrite);
        IntPtr fm = CreateFileMapping(fs.SafeFileHandle, IntPtr.Zero, 0x04, 0, 128*1024*1024, null); // PAGE_READWRITE=0x04
        IntPtr baseAddr = MapViewOfFile(fm, 0x0006, 0, 0, UIntPtr.Zero); // FILE_MAP_READ|WRITE
        long b = baseAddr.ToInt64();
        ulong f0 = faults();
        for (long i=0;i<128L*1024*1024;i+=4096) Marshal.WriteByte(new IntPtr(b+i), 1);
        ulong f1 = faults();
        SetProcessWorkingSetSize(GetCurrentProcess(), (IntPtr)(-1), (IntPtr)(-1)); // trim
        ulong f2 = faults();
        var ent = new WIN32_MEMORY_RANGE_ENTRY[1];
        ent[0].VirtualAddress = baseAddr; ent[0].NumberOfBytes = (UIntPtr)(128UL*1024*1024);
        bool ok = PrefetchVirtualMemory(GetCurrentProcess(), (UIntPtr)1, ent, 0);
        ulong f3 = faults();
        for (long i=0;i<128L*1024*1024;i+=4096) Marshal.ReadByte(new IntPtr(b+i));
        ulong f4 = faults();
        Console.WriteLine($"faults: touch={f1-f0} trim={f2-f1} prefetch(ok={ok})={f3-f2} re-touch={f4-f3}");
    }
}
