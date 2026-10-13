/* Text-only fallback: avoids copyin/scratch-buffer dereferences.
 * No device requests. Strings are bounded, NUL-terminated display evidence,
 * not exact packet bytes. Index/enable word and padding are not captured.
 */
#pragma D option strsize=320
#pragma D option bufsize=1m

dtrace:::BEGIN
{
    enables = 0; sends = 0; receives = 0; returns = 0; faults = 0;
    printf("TRACE_READY target=%d duration_seconds=10 mode=text-only\n", $target);
}

pid$target::IOConnectCallStructMethod:entry
/arg1 >= 173 && arg1 <= 175/
{
    self->selector = arg1;
    self->connection = arg0;
    /* Only read a reply when input/output share the verified 304-byte range. */
    self->reply = arg1 == 175 && arg3 == 304 && arg2 == arg4 ? arg4 : 0;
    printf("CALL time=%llu pid=%d tid=%d connection=%u selector=%u input_size=%llu\n",
        timestamp, pid, tid, (uint32_t)arg0, (uint32_t)arg1, (uint64_t)arg3);
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 173 && arg3 == 8/
{
    enables++;
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 174 && arg3 == 132 && arg2 != 0/
{
    sends++;
    printf("SEND_TEXT_BEGIN\n%s\nSEND_TEXT_END\n", copyinstr(arg2 + 4, 128));
    ustack(12);
}

pid$target::IOConnectCallStructMethod:entry
/arg1 == 175 && arg3 == 304/
{
    receives++;
}

pid$target::IOConnectCallStructMethod:return
/self->selector == 175 && arg1 == 0 && self->reply != 0/
{
    printf("RECEIVE_TEXT_BEGIN\n%s\nRECEIVE_TEXT_END\n", copyinstr(self->reply + 4, 300));
}

pid$target::IOConnectCallStructMethod:return
/self->selector != 0/
{
    returns++;
    printf("RETURN time=%llu pid=%d tid=%d connection=%u selector=%u result=0x%x\n",
        timestamp, pid, tid, (uint32_t)self->connection, (uint32_t)self->selector, (uint32_t)arg1);
    self->selector = 0; self->connection = 0; self->reply = 0;
}

dtrace:::ERROR
{
    faults++;
    printf("TRACE_FAULT probe=%d action=%d offset=%d fault=%d address=%p\n",
        (int)arg1, (int)arg2, (int)arg3, (int)arg4, (uintptr_t)arg5);
}

tick-10s { exit(0); }

dtrace:::END
{
    printf("TRACE_END enables=%d sends=%d gets=%d returns=%d faults=%d\n",
        enables, sends, receives, returns, faults);
}
