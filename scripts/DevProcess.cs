using System;
using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;

namespace UltraBar {
    // Create suspended, constrain the entire tree, then resume: rustc/link.exe never
    // get an unconstrained head start. Closing the job also cleans up its children.
    public static class DevProcess {
        [StructLayout(LayoutKind.Sequential)] struct BasicLimits {
            public long ProcessTime, JobTime;
            public uint Flags;
            public UIntPtr MinWorkingSet, MaxWorkingSet;
            public uint ActiveProcesses;
            public UIntPtr Affinity;
            public uint Priority, Scheduling;
        }
        [StructLayout(LayoutKind.Sequential)] struct IoCounters { public ulong A, B, C, D, E, F; }
        [StructLayout(LayoutKind.Sequential)] struct ExtendedLimits {
            public BasicLimits Basic;
            public IoCounters Io;
            public UIntPtr ProcessMemory, JobMemory, PeakProcessMemory, PeakJobMemory;
        }
        [StructLayout(LayoutKind.Sequential)] struct CpuRate { public uint Flags, Rate; }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct Startup {
            public uint Size;
            public string Reserved, Desktop, Title;
            public uint X, Y, Width, Height, XCount, YCount, Fill, Flags;
            public ushort Show, ReservedSize;
            public IntPtr ReservedPointer, Input, Output, Error;
        }
        [StructLayout(LayoutKind.Sequential)] struct ProcessInfo { public IntPtr Process, Thread; public uint Pid, Tid; }
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr CreateJobObjectW(IntPtr attributes, string name);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetInformationJobObject(IntPtr job, int kind, IntPtr value, uint size);
        [DllImport("kernel32.dll", SetLastError = true)] static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern bool CreateProcessW(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, bool inheritHandles, uint flags, IntPtr environment, string directory, ref Startup startup, out ProcessInfo info);
        [DllImport("kernel32.dll", SetLastError = true)] static extern uint ResumeThread(IntPtr thread);
        [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr handle, uint timeout);
        [DllImport("kernel32.dll")] static extern bool GetExitCodeProcess(IntPtr process, out uint code);
        [DllImport("kernel32.dll")] static extern bool TerminateProcess(IntPtr process, uint code);
        [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
        [DllImport("kernel32.dll")] static extern IntPtr GetStdHandle(int kind);

        static void Set<T>(IntPtr job, int kind, T value) {
            int size = Marshal.SizeOf(typeof(T));
            IntPtr buffer = Marshal.AllocHGlobal(size);
            try {
                Marshal.StructureToPtr(value, buffer, false);
                if (!SetInformationJobObject(job, kind, buffer, (uint)size)) throw new Win32Exception(Marshal.GetLastWin32Error());
            } finally { Marshal.FreeHGlobal(buffer); }
        }
        // Correct Windows argv quoting, including paths ending in a backslash.
        static string Quote(string value) {
            var output = new StringBuilder("\""); int slashes = 0;
            foreach (char c in value) {
                if (c == '\\') { slashes++; continue; }
                if (c == '"') output.Append('\\', slashes * 2 + 1);
                else output.Append('\\', slashes);
                output.Append(c); slashes = 0;
            }
            output.Append('\\', slashes * 2); output.Append('"'); return output.ToString();
        }
        public static int Run(string executable, string[] args, string directory, int percent, int cores) {
            IntPtr job = CreateJobObjectW(IntPtr.Zero, null);
            if (job == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error());
            ProcessInfo info = new ProcessInfo();
            try {
                ulong allowed = (ulong)Process.GetCurrentProcess().ProcessorAffinity.ToInt64();
                ulong affinity = 0;
                for (int bit = 0, count = 0; bit < IntPtr.Size * 8 && count < cores; bit++) {
                    ulong mask = 1UL << bit;
                    if ((allowed & mask) != 0) { affinity |= mask; count++; }
                }
                var limits = new ExtendedLimits();
                // Explicit breakaway is only used by the taskbar recovery watchdog.
                // KILL_ON_JOB_CLOSE | PRIORITY_CLASS | AFFINITY | BREAKAWAY_OK
                limits.Basic.Flags = 0x2000 | 0x20 | 0x10 | 0x800;
                limits.Basic.Affinity = new UIntPtr(affinity);
                limits.Basic.Priority = 0x4000; // BELOW_NORMAL_PRIORITY_CLASS
                Set(job, 9, limits);
                Set(job, 15, new CpuRate { Flags = 1 | 4, Rate = (uint)(percent * 100) });
                var startup = new Startup {
                    Size = (uint)Marshal.SizeOf(typeof(Startup)), Flags = 0x100,
                    Input = GetStdHandle(-10), Output = GetStdHandle(-11), Error = GetStdHandle(-12)
                };
                var command = new StringBuilder(Quote(executable));
                foreach (string arg in args) command.Append(' ').Append(Quote(arg));
                if (!CreateProcessW(executable, command, IntPtr.Zero, IntPtr.Zero, true,
                    0x4 | 0x4000, IntPtr.Zero, directory, ref startup, out info)) throw new Win32Exception(Marshal.GetLastWin32Error());
                if (!AssignProcessToJobObject(job, info.Process)) throw new Win32Exception(Marshal.GetLastWin32Error());
                if (ResumeThread(info.Thread) == uint.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
                WaitForSingleObject(info.Process, uint.MaxValue);
                uint code;
                if (!GetExitCodeProcess(info.Process, out code)) throw new Win32Exception(Marshal.GetLastWin32Error());
                return (int)code;
            } finally {
                if (info.Thread != IntPtr.Zero) CloseHandle(info.Thread);
                if (info.Process != IntPtr.Zero) { TerminateProcess(info.Process, 1); CloseHandle(info.Process); }
                CloseHandle(job);
            }
        }
    }
}
