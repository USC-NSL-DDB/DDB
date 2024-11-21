import re

def parse_times(text):
    # Initialize lists to store times
    pause_detected_timestamps = []
    timestamps=[]
    before_cont_timestamps = []
    pause_timestamps = []
    modify_times = []
    continue_times = []
    
    # Parse each line
    for line in text.split('\n'):
        if 'pause detected,' in line:
            timestamp = int(re.sub(r'\D', '', line.split()[-1].strip()))
            pause_detected_timestamps.append(timestamp)
        elif '~"timestamp:' in line:
            timestamp = int(re.search(r'timestamp: (\d+)', line).group(1))
            timestamps.append(timestamp)
        elif 'before cont time stamp' in line:
            before_cont_ts = int(line.split()[-1])
            before_cont_timestamps.append(before_cont_ts)
        elif 'pause time stamp' in line:
            pause_ts = int(line.split()[-1])
            pause_timestamps.append(pause_ts)
        elif 'modify_env_variable time:' in line:
            modify_time = float(re.search(r'time: ([\d.]+)', line).group(1))
            modify_times.append(modify_time)
        elif 'continue detected,' in line:
            continue_time = float(re.search(r'detected, ([\d.]+)', line).group(1))
            continue_times.append(continue_time)

    # Calculate time to pause for each interval
    time_to_pause = []
    time_to_cont = []
    for ts, pts in zip(pause_detected_timestamps, pause_timestamps):
        time_to_pause.append((ts - pts) / 1000000.0)  # Convert to ms
    for pts, bcts in zip(timestamps, before_cont_timestamps):
        time_to_cont.append((pts - bcts) / 1000000.0)
    # Print results
    print("Interval analysis:")
    total_pause = 0
    total_modify = 0
    total_continue = 0
    
    for i in range(len(time_to_pause)):
        print(f"\nInterval {i+1}:")
        print(f"Time to pause: {time_to_pause[i]:.3f} ms")
        print(f"Modify env time: {modify_times[i]:.3f} ms")
        print(f"Continue time: {continue_times[i]:.3f} ms")
        total_pause += time_to_pause[i]
        total_modify += modify_times[i]
        total_continue += continue_times[i]

    print("\nTotals:")
    
    print(f"Total time to pause: {total_pause:.3f} ms")
    print(f"Total time to cont: {sum(time_to_cont):.3f} ms")
    print(f"Total modify env time: {total_modify:.3f} ms") 
    print(f"Total continue time: {total_continue:.3f} ms")
    print(f"Total time: {total_pause + sum(time_to_cont) + total_modify + total_continue:.3f} ms")
with open ('res.out', 'r') as f:
    text = f.read()
    parse_times(text)