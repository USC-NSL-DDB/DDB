from collections import defaultdict
import requests
from datetime import datetime, timedelta, timezone
import matplotlib.pyplot as plt
import numpy as np

def get_trace_data():
    # Get the current time in UTC
    now = datetime.now(timezone.utc)

    # Calculate the time 24 hours ago
    twenty_four_hours_ago = now - timedelta(hours=1)

    # Format the times as RFC-3339 nanosecond strings
    end_time = now.strftime('%Y-%m-%dT%H:%M:%S.%f')[:-3] + 'Z'
    start_time = twenty_four_hours_ago.strftime('%Y-%m-%dT%H:%M:%S.%f')[:-3] + 'Z'

    # Construct the URL with the dynamic time range
    url = f'http://localhost:16686/api/v3/traces?query.start_time_min={start_time}&query.start_time_max={end_time}&query.service_name=example-service1'

    # Make the request
    response = requests.get(url).json()
    spans = response['result']['resourceSpans'][0]['scopeSpans'][0]['spans']
    valid_spans = []
    for span in spans:
        if span['name'] == 'process_command':
            for attribute in span['attributes']:
                if attribute['key'] == 'token':
                    span['token'] = int(attribute['value']['stringValue'])
                elif attribute['key'] == 'command':
                    span['command'] = attribute['value']['stringValue']
            valid_spans.append(span)
    
    for valid_span in valid_spans:
        for span in spans:
            if span['name'] == "process response":
                for attribute in span['attributes']:
                    if attribute['key'] == 'token':
                        if int(attribute['value']['stringValue']) == valid_span['token']:
                            valid_span['response'] = span
    
    result = []
    for valid_span in valid_spans:
        command = valid_span['command'].replace('\n', '')
        if 'response' not in valid_span:
            result.append({"command": command, "duration": -1})
        else:
            duration = -1
            for attribute in valid_span['response']['attributes']:
                if attribute['key'] == 'duration':
                    duration = float(attribute['value']['doubleValue'])
            result.append({"command": command, "duration": duration})
    result.sort(key=lambda x: x['duration'], reverse=True)
    with open('trace_data.txt', 'w') as f:
        for item in result:
            f.write(f"{item}\n")
    return result

def visualize_data(data):
    # Separate data into successful and missing responses
    successful = [item for item in data if item['duration'] != -1]
    missing = [item for item in data if item['duration'] == -1]

    # Sort successful responses by duration
    successful.sort(key=lambda x: x['duration'], reverse=True)

    # Create a figure with subplots
    fig, (ax1, ax2, ax3) = plt.subplots(3, 1, figsize=(12, 15))
    fig.suptitle('Trace Data Visualization', fontsize=16)

    # Bar chart for successful responses
    commands = [item['command'][:20] + '...' if len(item['command']) > 20 else item['command'] for item in successful]
    durations = [item['duration'] for item in successful]
    
    ax1.bar(commands, durations)
    ax1.set_title('Command Processing Durations')
    ax1.set_xlabel('Commands')
    ax1.set_ylabel('Duration (s)')
    ax1.tick_params(axis='x', rotation=45)

    # Pie chart for successful vs missing responses
    sizes = [len(successful), len(missing)]
    labels = f'Successful ({len(successful)})', f'Missing ({len(missing)})'
    colors = ['#66b3ff', '#ff9999']
    
    ax2.pie(sizes, labels=labels, colors=colors, autopct='%1.1f%%', startangle=90)
    ax2.set_title('Successful vs Missing Responses')

    # New section: Highlight missing commands
    missing_commands = [item['command'] for item in missing]
    y_pos = np.arange(len(missing_commands))
    
    ax3.barh(y_pos, [1]*len(missing_commands), align='center', color='#ff9999')
    ax3.set_yticks(y_pos)
    ax3.set_yticklabels(missing_commands)
    ax3.invert_yaxis()  # labels read top-to-bottom
    ax3.set_title('Commands with Missing Responses')
    ax3.set_xlabel('Missing Response')

    plt.tight_layout()
    plt.show()

    # Print missing responses
    if missing:
        print("\nCommands with Missing Responses:")
        for item in missing:
            print(f"- {item['command']}")

if __name__ == '__main__':
    trace_data = get_trace_data()
    visualize_data(trace_data)