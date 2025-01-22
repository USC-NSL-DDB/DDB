import os
import pty
import select
import signal
import subprocess
import sys
import threading
import termios
import time

def output_reader(fd, found_event):
    try:
        while True:
            ready, _, _ = select.select([fd], [], [], 0.1)
            if ready:
                try:
                    data = os.read(fd, 1024).decode()
                    if data:
                        sys.stdout.write(data)
                        sys.stdout.flush()
                        if "Will perform calibrated work cycles" in data:
                            found_event.set()
                        if "is not being run" in data:
                            break
                except OSError:
                    break
    except Exception as e:
        print(f"Reader error: {e}")
    finally:
        found_event.set()

def main():
    # Create a pseudo-terminal
    master_fd, slave_fd = pty.openpty()
    
    # Set raw mode on the slave to prevent echo
    term_settings = termios.tcgetattr(slave_fd)
    term_settings[3] = term_settings[3] & ~termios.ECHO
    termios.tcsetattr(slave_fd, termios.TCSANOW, term_settings)

    # Start GDB process with the pseudo-terminal
    gdb_process = subprocess.Popen(
        ['gdb', '/home/junzhouh/distributed_debugger/faketimetest/accuracytest/accuracy', '--quiet', '--nx'],
        stdin=slave_fd,
        stdout=slave_fd,
        stderr=slave_fd,
        preexec_fn=os.setsid
    )

    found_event = threading.Event()
    reader_thread = threading.Thread(target=output_reader, args=(master_fd, found_event))
    reader_thread.daemon = True
    reader_thread.start()
    
    try:
        # Send commands exactly as you would type them
        commands = [
            '\n',
            'source /home/junzhouh/distributed_debugger/faketimetest/bench/mysetup.gdb\n',
            'set pagination off\n',
            'run\n'
        ]
        
        for cmd in commands:
            os.write(master_fd, cmd.encode())
            time.sleep(0.1)  # Small delay between commands

        found_event.wait()
        print("Found event!")
        # for i in range(1, 10):
        #     time.sleep(1)
        #     os.killpg(gdb_process.pid, signal.SIGINT)
        #     time.sleep(2)
        #     os.write(master_fd, 'interpreter-exec mi "-record-time-and-continue"\n'.encode())
        while True and reader_thread.is_alive():
            time.sleep(1)
            print(f"pause time stamp {time.perf_counter_ns()}")
            os.killpg(gdb_process.pid, signal.SIGINT)
            time.sleep(2)
            print(f"before cont time stamp {time.perf_counter_ns()}")
            os.write(master_fd, 'interpreter-exec mi "-record-time-and-continue"\n'.encode())
    except KeyboardInterrupt:
        print("\nReceived interrupt, cleaning up...")
    finally:
        reader_thread.join()
        gdb_process.terminate()
        try:
            gdb_process.wait(timeout=1.0)
        except subprocess.TimeoutExpired:
            gdb_process.kill()
        
        os.close(master_fd)
        os.close(slave_fd)

if __name__ == "__main__":
    main()