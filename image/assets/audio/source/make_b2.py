exec(open('make_three.py').read().split('song_a("sample-A')[0])

def warm_piano(m, d):
    t = tarr(d + 0.8); f = hz(m)
    x = np.sin(2*np.pi*f*t) + 0.30*np.sin(2*np.pi*2*f*t)*np.exp(-t*3) + 0.10*np.sin(2*np.pi*3*f*t)*np.exp(-t*6)
    x *= 1 + 0.06*np.sin(2*np.pi*4.5*t)            # gentle tremolo
    return x * np.exp(-t*1.4) * np.minimum(1, t*120)

def song_b2(path, ver):
    bpm = 82; b = 60/bpm; bars = 8; m = Mix(bars*4*b)
    # plain, friendly chords: C - G/B - Am - F, played twice. Close voicings, each chord shares notes with the next
    ch = [(36, [60, 64, 67, 72]), (35, [59, 62, 67, 71]), (33, [60, 64, 69, 72]), (29, [60, 65, 69, 72])]*2
    if ver == 2: ch = [(36, [60, 64, 67, 72]), (31, [59, 62, 67, 74]), (33, [60, 64, 69, 72]), (29, [60, 65, 69, 76])]*2
    # melody sticks to the notes C D E G A, which sound right over every one of those chords
    mel = [(0, 76, 1.5), (1.5, 79, .5), (2, 81, 2),
           (4, 79, 1), (5, 76, 1), (6, 74, 2),
           (8, 72, 1), (9, 76, 1), (10, 81, 2),
           (12, 79, 1.5), (13.5, 76, .5), (14, 79, 2),
           (16, 76, 1.5), (17.5, 79, .5), (18, 84, 2),
           (20, 81, 1), (21, 79, 1), (22, 76, 2),
           (24, 74, 1), (25, 76, 1), (26, 79, 2),
           (28, 76, 3)]
    for bar, (root, notes) in enumerate(ch):
        t0 = bar*4*b
        for i, n in enumerate(notes): m.put(warm_piano(n, 3.2*b), t0 + i*0.008, 0.22, pan=-0.25 + i*0.17)
        m.put(warm_piano(notes[2], 1.0*b), t0 + 2.5*b, 0.10, pan=0.25)
        m.put(tri(root + 12, 1.7*b), t0, 0.70); m.put(tri(root + 12, 1.1*b), t0 + 2.5*b, 0.52)
        m.put(kick(), t0, 0.7); m.put(kick(), t0 + 2.5*b, 0.5)
        for k in (1, 3): m.put(snare(0.6), t0 + k*b, 0.26)
        for k in range(8): m.put(hat(0.45 if k % 2 else 0.3), t0 + k*b/2 + ((b/2)*0.18 if k % 2 else 0), 0.26, pan=0.25)
    for off, n, ln in mel: m.put(warm_piano(n, ln*b), off*b, 0.30, pan=0.15)
    m.save(path)

song_b2("sample-B2-warm-lofi-simple-chords.wav", 1)
song_b2("sample-B3-warm-lofi-open-chords.wav", 2)
