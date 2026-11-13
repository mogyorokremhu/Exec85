import numpy as np, wave, sys
SR = 44100
rng = np.random.default_rng(7)
def hz(m): return 440.0 * 2 ** ((m - 69) / 12)
def tarr(d): return np.arange(int(d * SR)) / SR
def adsr(n, a, d, s, r):
    e = np.ones(n); a_n, d_n, r_n = int(a*SR), int(d*SR), int(r*SR)
    if a_n: e[:a_n] = np.linspace(0, 1, a_n)
    if d_n and a_n + d_n <= n: e[a_n:a_n+d_n] = np.linspace(1, s, d_n); e[a_n+d_n:] = s
    if r_n and r_n < n: e[-r_n:] *= np.linspace(e[-r_n], 0, r_n)
    return e
def pulse(m, d, duty=0.25):
    t = tarr(d); ph = (hz(m) * t) % 1.0
    return np.where(ph < duty, 1.0, -1.0) * adsr(len(t), 0.003, 0.05, 0.6, 0.04)
def tri(m, d):
    t = tarr(d); ph = (hz(m) * t) % 1.0
    return (4 * np.abs(ph - 0.5) - 1) * adsr(len(t), 0.003, 0.02, 0.9, 0.03)
def epiano(m, d):
    t = tarr(d + 0.6); f = hz(m)
    x = np.sin(2*np.pi*f*t) + 0.5*np.sin(2*np.pi*2*f*t)*np.exp(-t*4) + 0.25*np.sin(2*np.pi*3*f*t)*np.exp(-t*7) + 0.08*np.sin(2*np.pi*7*f*t)*np.exp(-t*20)
    return x * np.exp(-t*1.8) * np.minimum(1, t*300)
def pluck(m, d):
    t = tarr(d + 0.3); f = hz(m)
    x = sum(np.sin(2*np.pi*f*k*t) / k * np.exp(-t*(3 + k*2.5)) for k in range(1, 7))
    return x * np.minimum(1, t*500)
def kick(vel=1.0):
    t = tarr(0.22); f = 50 + 120*np.exp(-t*28)
    return np.sin(2*np.pi*np.cumsum(f)/SR) * np.exp(-t*14) * vel
def snare(vel=1.0):
    t = tarr(0.18); n = rng.normal(size=len(t))
    return (n*0.7*np.exp(-t*22) + np.sin(2*np.pi*190*t)*0.5*np.exp(-t*30)) * vel
def hat(vel=0.5, d=0.05):
    t = tarr(d); n = rng.normal(size=len(t)); n = n - np.concatenate([[0], n[:-1]])
    return n * np.exp(-t*(90 if d < 0.08 else 25)) * vel

class Mix:
    def __init__(self, secs): self.loop = int(round(secs*SR)); self.L = np.zeros(self.loop + 6*SR); self.R = np.zeros_like(self.L)
    def put(self, x, t0, g=1.0, pan=0.0):
        i = int(t0*SR); j = min(i+len(x), len(self.L))
        if i >= len(self.L) or j <= i: return
        self.L[i:j] += x[:j-i]*g*(1-pan)*0.7; self.R[i:j] += x[:j-i]*g*(1+pan)*0.7
    def save(self, path, fade=0):
        # seamless loop: whatever rings on past the end is folded back onto the start
        n = self.loop; L, R = self.L, self.R
        tail = len(L) - n
        L2 = L[:n].copy(); R2 = R[:n].copy(); L2[:tail] += L[n:]; R2[:tail] += R[n:]
        st = np.stack([L2, R2], 1)
        st = np.tanh(st*1.1); st = st/np.max(np.abs(st))*0.8
        with wave.open(path, "wb") as w:
            w.setnchannels(2); w.setsampwidth(2); w.setframerate(SR); w.writeframes((st*32767).astype(np.int16).tobytes())

# A: retro chiptune, 124 BPM, C - G - Am - F
def song_a(path):
    bpm = 124; b = 60/bpm; bars = 8; m = Mix(bars*4*b)
    prog = [(48, [60, 64, 67]), (43, [59, 62, 67]), (45, [57, 60, 64]), (41, [57, 60, 65])]*2
    mel = [[76, 79, 84, 79, 76, 79, 83, 79], [74, 79, 83, 79, 74, 79, 83, 86], [72, 76, 81, 76, 72, 76, 81, 84], [72, 77, 81, 77, 72, 77, 81, 77],
           [79, 83, 86, 91, 88, 86, 83, 79], [78, 83, 86, 90, 86, 83, 78, 83], [81, 84, 88, 93, 88, 84, 81, 84], [84, 81, 77, 81, 84, 89, 88, 84]]
    for bar, (root, tri_notes) in enumerate(prog):
        t0 = bar*4*b
        for k in range(8): m.put(tri(root + (12 if k % 4 == 2 else 0), b/2*0.9), t0 + k*b/2, 0.55)
        for k in range(16): m.put(pulse(tri_notes[k % 3] + 12, b/4*0.8, 0.125), t0 + k*b/4, 0.10, pan=-0.4 if k % 2 else 0.4)
        for k in range(8): m.put(pulse(mel[bar][k], b/2*0.92, 0.25), t0 + k*b/2, 0.22, pan=0.1)
        for k in range(4): m.put(kick(), t0 + k*b, 0.7)
        for k in (1, 3): m.put(snare(), t0 + k*b, 0.45)
        for k in range(8): m.put(hat(0.4 if k % 2 else 0.25), t0 + k*b/2 + (0 if k % 2 == 0 else 0), 0.35)
    m.save(path)

# B: warm lo-fi, 82 BPM, swung, electric piano chords
def song_b(path):
    bpm = 82; b = 60/bpm; bars = 8; m = Mix(bars*4*b)
    ch = [(48, [64, 67, 71, 74]), (45, [64, 67, 72, 76]), (50, [65, 69, 72, 76]), (43, [65, 69, 71, 77])]*2
    mel = [(0, 83, 1), (1, 79, .5), (1.5, 81, .5), (2, 83, 2), (4, 81, 1.5), (5.5, 76, .5), (6, 79, 2), (8, 77, 1), (9, 81, 1), (10, 84, 2), (12, 83, 1.5), (13.5, 79, .5), (14, 74, 2)]
    for bar, (root, notes) in enumerate(ch):
        t0 = bar*4*b
        for i, n in enumerate(notes): m.put(epiano(n, 3*b), t0 + i*0.012, 0.22, pan=-0.3 + i*0.2)
        m.put(epiano(notes[1], 1.2*b), t0 + 2.5*b, 0.12, pan=0.3)
        m.put(tri(root - 12 if root > 44 else root, 1.8*b), t0, 0.7); m.put(tri(root - 12 if root > 44 else root, 1.2*b), t0 + 2.5*b, 0.55)
        m.put(kick(), t0, 0.7); m.put(kick(), t0 + 2.5*b, 0.55)
        for k in (1, 3): m.put(snare(0.7), t0 + k*b, 0.3)
        for k in range(8):
            sw = (b/2)*0.18 if k % 2 else 0
            m.put(hat(0.45 if k % 2 else 0.3), t0 + k*b/2 + sw, 0.3, pan=0.25)
    for off, n, ln in mel[:7] if False else mel:
        m.put(epiano(n + 0, ln*b), 0*0 + off*b, 0.30, pan=0.15)
    m.save(path)

# C: bright synth-pop, 112 BPM, C - G - Am - F, plucks and four-on-the-floor
def song_c(path):
    bpm = 112; b = 60/bpm; bars = 8; m = Mix(bars*4*b)
    prog = [(48, [72, 76, 79, 84]), (43, [71, 74, 79, 83]), (45, [72, 76, 81, 84]), (41, [72, 77, 81, 84])]*2
    hook = [(0, 88, .5), (.5, 91, .5), (1, 88, 1), (2, 84, .5), (2.5, 86, .5), (3, 88, 1)]
    for bar, (root, notes) in enumerate(prog):
        t0 = bar*4*b
        for k in range(8): m.put(pluck(notes[(k*3) % 4], b/2), t0 + k*b/2, 0.30, pan=-0.45 + 0.13*k)
        for k in range(4):
            m.put(kick(), t0 + k*b, 0.8)
            m.put(tri(root, b*0.45), t0 + k*b, 0.6); m.put(tri(root + 12, b*0.45), t0 + k*b + b/2, 0.45)
        for k in (1, 3): m.put(snare(), t0 + k*b, 0.4)
        for k in range(4): m.put(hat(0.5, 0.09), t0 + k*b + b/2, 0.4)
        if bar >= 2:
            for off, n, ln in hook: m.put(pluck(n if bar % 4 < 2 else n - 2, ln*b), t0 + off*b, 0.30, pan=0.2)
    m.save(path)

song_a("sample-A-retro-chiptune.wav"); song_b("sample-B-warm-lofi.wav"); song_c("sample-C-bright-pop.wav")
