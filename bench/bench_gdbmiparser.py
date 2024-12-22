from pprint import pformat
from typing import Optional,Tuple,Dict,List,Any
from pygdbmi import gdbmiparser
from iddb.logging import logger
import time

def _buffer_incomplete_responses(
raw_output: Optional[bytes], buf: Optional[bytes]
) -> Tuple[Optional[bytes], Optional[bytes]]:
    if raw_output:
        if buf:
            # concatenate buffer and new output
            raw_output = b"".join([buf, raw_output])
            buf = None

        if b"\n" not in raw_output:
            # newline was not found, so assume output is incomplete and store in buffer
            buf = raw_output
            raw_output = None

        elif not raw_output.endswith(b"\n"):
            # raw output doesn't end in a newline, so store everything after the last newline (if anything)
            # in the buffer, and parse everything before it
            remainder_offset = raw_output.rindex(b"\n") + 1
            buf = raw_output[remainder_offset:]
            raw_output = raw_output[:remainder_offset]
    return (raw_output, buf)

class GdbParser:
    def __init__(self,verbose=False):
        self._incomplete_output={"stdout":None,"stderr":None}
        self.verbose=verbose

    def get_responses_list(
        self, raw_output: bytes, stream: str
    ) -> List[Dict[Any, Any]]:
        """Get parsed response list from string output
        Args:
            raw_output (unicode): gdb output to parse
            stream (str): either stdout or stderr
        """
        responses: List[Dict[Any, Any]] = []

        (_new_output, self._incomplete_output[stream],) = _buffer_incomplete_responses(
            raw_output, self._incomplete_output.get(stream)
        )

        if not _new_output:
            return responses

        response_list = list(
            filter(lambda x: x, _new_output.decode(errors="replace").split("\n"))
        )  # remove blank lines

        # parse each response from gdb into a dict, and store in a list
        for response in response_list:
            if gdbmiparser.response_is_finished(response):
                pass
            else:
                parsed_response = gdbmiparser.parse_response(response)
                parsed_response["stream"] = stream
                if self.verbose:
                    logger.debug(pformat(parsed_response))
                responses.append(parsed_response)
        return responses

        
def benchmark_get_responses_list(parser: GdbParser, raw_output: bytes, stream: str, iterations: int = 1000) -> None:
    start_time = time.time()
    for _ in range(iterations):
        parser.get_responses_list(raw_output, stream)
    end_time = time.time()
    elapsed_time = end_time - start_time
    print(f"Benchmark completed in {elapsed_time*1000:.4f} ms for {iterations} iterations.")
    print(f"Time per iteration: {(elapsed_time/iterations)*1000:.4f} ms")

if __name__ == "__main__":
    parser = GdbParser(verbose=False)

    stream = "stdout"
    gdb_mi_examples = [
        b'^done,frame={level="0",addr="0x0000000000400544",func="main",file="example.c",fullname="/home/user/example.c",line="5"}',
        b'*stopped,reason="breakpoint-hit",disp="keep",bkptno="1",thread-id="1",frame={addr="0x0000000000400544",func="main",args=[],file="example.c",line="5"}',
        b'^done,BreakpointTable={nr_rows="2",nr_cols="6",hdr=[{width="7",alignment="-1",col_name="number",colhdr="Num"},{width="14",alignment="-1",col_name="type",colhdr="Type"},{width="4",alignment="-1",col_name="disp",colhdr="Disp"},{width="3",alignment="-1",col_name="enabled",colhdr="Enb"},{width="18",alignment="-1",col_name="addr",colhdr="Address"},{width="40",alignment="2",col_name="what",colhdr="What"}]}',
        b'=breakpoint-modified,bkpt={number="1",type="breakpoint",disp="keep",enabled="y",addr="0x0000000000400544",func="main",file="example.c",line="5",thread-groups=["i1"]}',
        b'^done,stack=[frame={level="0",addr="0x0000000000400544",func="main",file="example.c",line="5"},frame={level="1",addr="0x00007ffff7a52740",func="__libc_start_main"}]',
        b'^done,variables=[{name="var1",value="10"},{name="var2",value="hello"},{name="var3",value="0x7fffffffe4a0"}]'
        b'2372^done,stack=[frame={level="0",addr="0x00007ffff792725d",func="syscall",file="../sysdeps/unix/sysv/linux/x86_64/syscall.S",fullname="./misc/../sysdeps/unix/sysv/linux/x86_64/syscall.S",line="38",arch="i386:x86-64"},frame={level="1",addr="0x000055555661f079",func="absl::lts_20240116::synchronization_internal::FutexImpl::WaitRelativeTimeout(std::atomic<int>*, int, timespec const*)",arch="i386:x86-64"},frame={level="2",addr="0x000055555661ec2a",func="absl::lts_20240116::synchronization_internal::FutexWaiter::WaitUntil(std::atomic<int>*, int, absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="3",addr="0x000055555661ed9d",func="absl::lts_20240116::synchronization_internal::FutexWaiter::Wait(absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="4",addr="0x000055555661ea58",func="AbslInternalPerThreadSemWait_lts_20240116",arch="i386:x86-64"},frame={level="5",addr="0x000055555661d24a",func="absl::lts_20240116::synchronization_internal::PerThreadSem::Wait(absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="6",addr="0x0000555556615087",func="absl::lts_20240116::Mutex::DecrementSynchSem(absl::lts_20240116::Mutex*, absl::lts_20240116::base_internal::PerThreadSynch*, absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="7",addr="0x000055555661c3d1",func="absl::lts_20240116::CondVar::WaitCommon(absl::lts_20240116::Mutex*, absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="8",addr="0x0000555555d51491",func="absl::lts_20240116::CondVar::WaitWithTimeout(absl::lts_20240116::Mutex*, absl::lts_20240116::Duration)",arch="i386:x86-64"},frame={level="9",addr="0x000055555604447a",func="grpc_event_engine::experimental::WorkStealingThreadPool::WorkSignal::WaitWithTimeout(grpc_core::Duration)",arch="i386:x86-64"},frame={level="10",addr="0x0000555556043e5c",func="grpc_event_engine::experimental::WorkStealingThreadPool::ThreadState::Step()",arch="i386:x86-64"},frame={level="11",addr="0x0000555556043919",func="grpc_event_engine::experimental::WorkStealingThreadPool::ThreadState::ThreadBody()",arch="i386:x86-64"},frame={level="12",addr="0x00005555560421ad",func="grpc_event_engine::experimental::WorkStealingThreadPool::WorkStealingThreadPoolImpl::StartThread()::{lambda(void*)#1}::operator()(void*) const",arch="i386:x86-64"},frame={level="13",addr="0x00005555560421f3",func="grpc_event_engine::experimental::WorkStealingThreadPool::WorkStealingThreadPoolImpl::StartThread()::{lambda(void*)#1}::_FUN(void*)",arch="i386:x86-64"},frame={level="14",addr="0x0000555556435722",func="grpc_core::(anonymous namespace)::ThreadInternalsPosix::ThreadInternalsPosix(char const*, void (*)(void*), void*, bool*, grpc_core::Thread::Options const&)::{lambda(void*)#1}::operator()(void*) const",arch="i386:x86-64"},frame={level="15",addr="0x000055555643576f",func="grpc_core::(anonymous namespace)::ThreadInternalsPosix::ThreadInternalsPosix(char const*, void (*)(void*), void*, bool*, grpc_core::Thread::Options const&)::{lambda(void*)#1}::_FUN(void*)",arch="i386:x86-64"},frame={level="16",addr="0x00007ffff789ca94",func="start_thread",file="./nptl/pthread_create.c",fullname="./nptl/./nptl/pthread_create.c",line="447",arch="i386:x86-64"},frame={level="17",addr="0x00007ffff7929c3c",func="clone3",file="../sysdeps/unix/sysv/linux/x86_64/clone3.S",fullname="./misc/../sysdeps/unix/sysv/linux/x86_64/clone3.S",line="78",arch="i386:x86-64"}]\n',
        b'2292^done,stack=[frame={level="0",addr="0x00007ffff7898d61",func="__futex_abstimed_wait_common64",file="./nptl/futex-internal.c",fullname="./nptl/./nptl/futex-internal.c",line="57",arch="i386:x86-64"},frame={level="1",addr="0x00007ffff7898d61",func="__futex_abstimed_wait_common",file="./nptl/futex-internal.c",fullname="./nptl/./nptl/futex-internal.c",line="87",arch="i386:x86-64"},frame={level="2",addr="0x00007ffff7898d61",func="__GI___futex_abstimed_wait_cancelable64",file="./nptl/futex-internal.c",fullname="./nptl/./nptl/futex-internal.c",line="139",arch="i386:x86-64"},frame={level="3",addr="0x00007ffff789b7dd",func="__pthread_cond_wait_common",file="./nptl/pthread_cond_wait.c",fullname="./nptl/./nptl/pthread_cond_wait.c",line="503",arch="i386:x86-64"},frame={level="4",addr="0x00007ffff789b7dd",func="___pthread_cond_wait",file="./nptl/pthread_cond_wait.c",fullname="./nptl/./nptl/pthread_cond_wait.c",line="627",arch="i386:x86-64"},frame={level="5",addr="0x000055555572a893",func="std::condition_variable::wait<spdlog::details::mpmc_blocking_queue<spdlog::details::async_msg>::dequeue(spdlog::details::async_msg&)::{lambda()#1}>(std::unique_lock<std::mutex>&, spdlog::details::mpmc_blocking_queue<spdlog::details::async_msg>::dequeue(spdlog::details::async_msg&)::{lambda()#1})",file="/usr/include/c++/13/condition_variable",fullname="/usr/include/c++/13/condition_variable",line="105",arch="i386:x86-64"},frame={level="6",addr="0x0000555555729eee",func="spdlog::details::mpmc_blocking_queue<spdlog::details::async_msg>::dequeue",file="/home/ybyan/proj/distributed-debugger/adapter/sampleWorkSpace/raft/libs/spdlog/include/spdlog/details/mpmc_blocking_q.h",fullname="/home/ybyan/proj/distributed-debugger/apps/raft/libs/spdlog/include/spdlog/details/mpmc_blocking_q.h",line="85",arch="i386:x86-64"},frame={level="7",addr="0x000055555572725c",func="spdlog::details::thread_pool::process_next_msg_",file="/home/ybyan/proj/distributed-debugger/adapter/sampleWorkSpace/raft/libs/spdlog/include/spdlog/details/thread_pool-inl.h",fullname="/home/ybyan/proj/distributed-debugger/apps/raft/libs/spdlog/include/spdlog/details/thread_pool-inl.h",line="106",arch="i386:x86-64"},frame={level="8",addr="0x0000555555727205",func="spdlog::details::thread_pool::worker_loop_",file="/home/ybyan/proj/distributed-debugger/adapter/sampleWorkSpace/raft/libs/spdlog/include/spdlog/details/thread_pool-inl.h",fullname="/home/ybyan/proj/distributed-debugger/apps/raft/libs/spdlog/include/spdlog/details/thread_pool-inl.h",line="97",arch="i386:x86-64"},frame={level="9",addr="0x0000555555726811",func="operator()",file="/home/ybyan/proj/distributed-debugger/adapter/sampleWorkSpace/raft/libs/spdlog/include/spdlog/details/thread_pool-inl.h",fullname="/home/ybyan/proj/distributed-debugger/apps/raft/libs/spdlog/include/spdlog/details/thread_pool-inl.h",line="29",arch="i386:x86-64"},frame={level="10",addr="0x000055555572878a",func="std::__invoke_impl<void, spdlog::details::thread_pool::thread_pool(size_t, size_t, std::function<void()>, std::function<void()>)::<lambda()> >(std::__invoke_other, struct {...} &&)",file="/usr/include/c++/13/bits/invoke.h",fullname="/usr/include/c++/13/bits/invoke.h",line="61",arch="i386:x86-64"},frame={level="11",addr="0x000055555572874d",func="std::__invoke<spdlog::details::thread_pool::thread_pool(size_t, size_t, std::function<void()>, std::function<void()>)::<lambda()> >(struct {...} &&)",file="/usr/include/c++/13/bits/invoke.h",fullname="/usr/include/c++/13/bits/invoke.h",line="96",arch="i386:x86-64"},frame={level="12",addr="0x00005555557286fa",func="std::thread::_Invoker<std::tuple<spdlog::details::thread_pool::thread_pool(size_t, size_t, std::function<void()>, std::function<void()>)::<lambda()> > >::_M_invoke<0>(std::_Index_tuple<0>)",file="/usr/include/c++/13/bits/std_thread.h",fullname="/usr/include/c++/13/bits/std_thread.h",line="292",arch="i386:x86-64"},frame={level="13",addr="0x00005555557286ce",func="std::thread::_Invoker<std::tuple<spdlog::details::thread_pool::thread_pool(size_t, size_t, std::function<void()>, std::function<void()>)::<lambda()> > >::operator()(void)",file="/usr/include/c++/13/bits/std_thread.h",fullname="/usr/include/c++/13/bits/std_thread.h",line="299",arch="i386:x86-64"},frame={level="14",addr="0x00005555557286b2",func="std::thread::_State_impl<std::thread::_Invoker<std::tuple<spdlog::details::thread_pool::thread_pool(size_t, size_t, std::function<void()>, std::function<void()>)::<lambda()> > > >::_M_run(void)",file="/usr/include/c++/13/bits/std_thread.h",fullname="/usr/include/c++/13/bits/std_thread.h",line="244",arch="i386:x86-64"},frame={level="15",addr="0x00007ffff7cecdb4",func="??",from="/lib/x86_64-linux-gnu/libstdc++.so.6",arch="i386:x86-64"},frame={level="16",addr="0x00007ffff789ca94",func="start_thread",file="./nptl/pthread_create.c",fullname="./nptl/./nptl/pthread_create.c",line="447",arch="i386:x86-64"},frame={level="17",addr="0x00007ffff7929c3c",func="clone3",file="../sysdeps/unix/sysv/linux/x86_64/clone3.S",fullname="./misc/../sysdeps/unix/sysv/linux/x86_64/clone3.S",line="78",arch="i386:x86-64"}]\n',
        b'1438^done,stack=[frame={level="0",addr="0x00007ffff792725d",func="syscall",file="../sysdeps/unix/sysv/linux/x86_64/syscall.S",fullname="./misc/../sysdeps/unix/sysv/linux/x86_64/syscall.S",line="38",arch="i386:x86-64"},frame={level="1",addr="0x000055555661efe5",func="absl::lts_20240116::synchronization_internal::FutexImpl::WaitAbsoluteTimeout(std::atomic<int>*, int, timespec const*)",arch="i386:x86-64"},frame={level="2",addr="0x000055555661ef78",func="absl::lts_20240116::synchronization_internal::FutexImpl::Wait(std::atomic<int>*, int)",arch="i386:x86-64"},frame={level="3",addr="0x000055555661ebd6",func="absl::lts_20240116::synchronization_internal::FutexWaiter::WaitUntil(std::atomic<int>*, int, absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="4",addr="0x000055555661ed9d",func="absl::lts_20240116::synchronization_internal::FutexWaiter::Wait(absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="5",addr="0x000055555661ea58",func="AbslInternalPerThreadSemWait_lts_20240116",arch="i386:x86-64"},frame={level="6",addr="0x000055555661d24a",func="absl::lts_20240116::synchronization_internal::PerThreadSem::Wait(absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="7",addr="0x0000555556615087",func="absl::lts_20240116::Mutex::DecrementSynchSem(absl::lts_20240116::Mutex*, absl::lts_20240116::base_internal::PerThreadSynch*, absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="8",addr="0x000055555661c3d1",func="absl::lts_20240116::CondVar::WaitCommon(absl::lts_20240116::Mutex*, absl::lts_20240116::synchronization_internal::KernelTimeout)",arch="i386:x86-64"},frame={level="9",addr="0x0000555555786e8b",func="absl::lts_20240116::CondVar::Wait(absl::lts_20240116::Mutex*)",arch="i386:x86-64"},frame={level="10",addr="0x00005555557857a8",func="grpc::Server::Wait()",arch="i386:x86-64"},frame={level="11",addr="0x00005555556244c4",func="rafty::Raft::start_server()::{lambda()#1}::operator()() const",file="/home/ybyan/proj/distributed-debugger/adapter/sampleWorkSpace/raft/inc/rafty/impl/raft.ipp",fullname="/home/ybyan/proj/distributed-debugger/apps/raft/inc/rafty/impl/raft.ipp",line="26",arch="i386:x86-64"},frame={level="12",addr="0x0000555555652b2a",func="std::__invoke_impl<void, rafty::Raft::start_server()::{lambda()#1}>(std::__invoke_other, rafty::Raft::start_server()::{lambda()#1}&&)",file="/usr/include/c++/13/bits/invoke.h",fullname="/usr/include/c++/13/bits/invoke.h",line="61",arch="i386:x86-64"},frame={level="13",addr="0x000055555565209b",func="std::__invoke<rafty::Raft::start_server()::{lambda()#1}>(rafty::Raft::start_server()::{lambda()#1}&&)",file="/usr/include/c++/13/bits/invoke.h",fullname="/usr/include/c++/13/bits/invoke.h",line="96",arch="i386:x86-64"},frame={level="14",addr="0x00005555556507ba",func="std::thread::_Invoker<std::tuple<rafty::Raft::start_server()::{lambda()#1}> >::_M_invoke<0ul>(std::_Index_tuple<0ul>)",file="/usr/include/c++/13/bits/std_thread.h",fullname="/usr/include/c++/13/bits/std_thread.h",line="292",arch="i386:x86-64"},frame={level="15",addr="0x000055555564a1d2",func="std::thread::_Invoker<std::tuple<rafty::Raft::start_server()::{lambda()#1}> >::operator()()",file="/usr/include/c++/13/bits/std_thread.h",fullname="/usr/include/c++/13/bits/std_thread.h",line="299",arch="i386:x86-64"},frame={level="16",addr="0x0000555555649628",func="std::thread::_State_impl<std::thread::_Invoker<std::tuple<rafty::Raft::start_server()::{lambda()#1}> > >::_M_run()",file="/usr/include/c++/13/bits/std_thread.h",fullname="/usr/include/c++/13/bits/std_thread.h",line="244",arch="i386:x86-64"},frame={level="17",addr="0x00007ffff7cecdb4",func="??",from="/lib/x86_64-linux-gnu/libstdc++.so.6",arch="i386:x86-64"},frame={level="18",addr="0x00007ffff789ca94",func="start_thread",file="./nptl/pthread_create.c",fullname="./nptl/./nptl/pthread_create.c",line="447",arch="i386:x86-64"},frame={level="19",addr="0x00007ffff7929c3c",func="clone3",file="../sysdeps/unix/sysv/linux/x86_64/clone3.S",fullname="./misc/../sysdeps/unix/sysv/linux/x86_64/clone3.S",line="78",arch="i386:x86-64"}]\n'
    ]
    itr = 1000
    start = time.time()
    for example in gdb_mi_examples:
        benchmark_get_responses_list(parser, example, stream, iterations=itr)
    end = time.time()
    print(f"Total time: {end-start} seconds for {len(gdb_mi_examples)*itr} iterations.")
    print(f"Average time: {(end-start)/(len(gdb_mi_examples)*itr)} seconds for 1 iteration.")
    print(f"Operations per second: {(len(gdb_mi_examples)*itr)/(end-start):.2f}")
    print(f"Average time per operation: {((end-start)/(len(gdb_mi_examples)*itr))*1000:.3f} ms")
    