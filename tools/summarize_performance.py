#!/usr/bin/env python3
"""Summarize raw frame batches; nearest-rank p95, one warm-up excluded by collector."""
import collections, json, math, statistics, sys

def summarize(path):
    data = json.load(open(path))
    groups = collections.defaultdict(list)
    repeats = collections.defaultdict(list)
    for row in data['records']:
        groups[row['workload']].extend(row['frames'])
        times = [f['ms'] for f in row['frames']]
        repeats[row['workload']].append({'repetition': row['repetition'], 'samples': len(times),
            'median_ms': statistics.median(times) if times else None})
    result = {}
    for name, frames in groups.items():
        times = sorted(f['ms'] for f in frames)
        result[name] = {'samples': len(times), 'median_ms': statistics.median(times),
            'p95_ms': times[math.ceil(.95 * len(times)) - 1],
            'mean_painted_pixels': round(statistics.mean(f['pixels'] for f in frames), 1),
            'frame_kinds': dict(collections.Counter(f['kind'] for f in frames)),
            'repetitions': repeats[name]}
    return result
if __name__ == '__main__':
    print(json.dumps({p: summarize(p) for p in sys.argv[1:]}, indent=2))
