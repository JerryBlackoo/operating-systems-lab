#!/usr/bin/env python3
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import csv
import os

os.chdir('/home/root1/scheduler_lab')

algorithms = ['FCFS', 'SJF', 'RR', 'CFS']
turnaround = []
response = []

for alg in algorithms:
    filename = f"{alg}_results.csv"
    if os.path.exists(filename):
        with open(filename, 'r') as f:
            reader = csv.DictReader(f)
            for row in reader:
                turnaround.append(float(row['平均周转时间']))
                response.append(float(row['平均响应时间']))
                break

if len(turnaround) == 0:
    print("No CSV files found. Please run ./simulator first.")
    exit(1)

x = range(len(algorithms))
width = 0.35

fig, (ax1, ax2) = plt.subplots(1, 2, figsize=(12, 5))

ax1.bar(x, turnaround, width, color='skyblue')
ax1.set_xlabel('Scheduling Algorithm')
ax1.set_ylabel('Time (seconds)')
ax1.set_title('Average Turnaround Time Comparison')
ax1.set_xticks(x)
ax1.set_xticklabels(algorithms)
ax1.grid(True, linestyle='--', alpha=0.7)

ax2.bar(x, response, width, color='lightcoral')
ax2.set_xlabel('Scheduling Algorithm')
ax2.set_ylabel('Time (seconds)')
ax2.set_title('Average Response Time Comparison')
ax2.set_xticks(x)
ax2.set_xticklabels(algorithms)
ax2.grid(True, linestyle='--', alpha=0.7)

plt.tight_layout()
plt.savefig('scheduler_comparison.png', dpi=150)
print("Chart saved as scheduler_comparison.png")