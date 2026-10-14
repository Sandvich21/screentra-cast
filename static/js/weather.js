
        // ── WMO weather code → description + inline SVG icon ──
        // SVG icons are used instead of emoji because emoji fonts are often
        // missing on minimal Linux installs (e.g. CCCTC players), causing them
        // to render as blank squares or fallback glyphs. SVGs render reliably
        // across all platforms and scale crisply at any size.
        //
        // Each icon is a self-contained <svg> with viewBox 0 0 64 64. Colors are
        // chosen for visibility on a dark/light background — yellow sun, blue
        // raindrops, white clouds, etc. They're sized to 1em-equivalent and
        // scale with the icon container.
        const WMO_CODES = {
            0:  { desc: 'Clear',          icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Clear"><circle cx="32" cy="32" r="14" fill="#FFD23F"/><g stroke="#FFD23F" stroke-width="4" stroke-linecap="round"><line x1="32" y1="4"  x2="32" y2="12"/><line x1="32" y1="52" x2="32" y2="60"/><line x1="4"  y1="32" x2="12" y2="32"/><line x1="52" y1="32" x2="60" y2="32"/><line x1="11" y1="11" x2="17" y2="17"/><line x1="47" y1="47" x2="53" y2="53"/><line x1="11" y1="53" x2="17" y2="47"/><line x1="47" y1="17" x2="53" y2="11"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Clear night"><circle cx="32" cy="36" r="16" fill="#C4C4D4"/><path d="M26 18 Q20 30 24 36 Q18 34 16 42 Q22 44 28 40 Q24 50 34 52 Q38 44 36 36 Q44 42 44 32 Q40 22 30 24 Q28 18 26 18 Z" fill="#C4C4D4"/><circle cx="44" cy="18" r="2" fill="#C4C4D4"/><circle cx="50" cy="28" r="1.2" fill="#C4C4D4"/><circle cx="16" cy="28" r="1.5" fill="#C4C4D4"/></svg>' },
            1:  { desc: 'Mainly clear',   icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Mainly clear"><circle cx="22" cy="22" r="10" fill="#FFD23F"/><g stroke="#FFD23F" stroke-width="3" stroke-linecap="round"><line x1="22" y1="4"  x2="22" y2="9"/><line x1="4"  y1="22" x2="9"  y2="22"/><line x1="9"  y1="9"  x2="13" y2="13"/><line x1="35" y1="9"  x2="31" y2="13"/></g><path d="M20 44 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="2"/></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Mainly clear night"><path d="M20 44 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><path d="M18 14 Q12 26 16 32 Q10 30 8 38 Q14 40 20 36 Q16 46 26 48 Q30 40 28 32 Q36 38 36 28 Q32 18 22 20 Q20 14 18 14 Z" fill="#C4C4D4"/></svg>' },
            2:  { desc: 'Partly cloudy',  icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Partly cloudy"><circle cx="22" cy="20" r="9" fill="#FFD23F"/><g stroke="#FFD23F" stroke-width="3" stroke-linecap="round"><line x1="22" y1="4"  x2="22" y2="8"/><line x1="4"  y1="20" x2="8"  y2="20"/><line x1="9"  y1="7"  x2="12" y2="10"/><line x1="35" y1="10" x2="32" y2="13"/></g><path d="M18 46 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="2"/></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Partly cloudy night"><path d="M18 46 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><path d="M20 12 Q14 24 18 30 Q12 28 10 36 Q16 38 22 34 Q18 44 28 46 Q32 38 30 30 Q38 36 38 26 Q34 16 24 18 Q22 12 20 12 Z" fill="#C4C4D4"/></svg>' },
            3:  { desc: 'Overcast',       icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Overcast"><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><path d="M8 48 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#B8C0C8" stroke="#6B7480" stroke-width="1.5"/></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Overcast night"><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><path d="M8 48 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#6B7480" stroke="#4A5058" stroke-width="1.5"/></svg>' },
            45: { desc: 'Fog',            icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Fog"><path d="M14 32 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="1.5"/><g stroke="#9DA5AD" stroke-width="3" stroke-linecap="round"><line x1="6"  y1="46" x2="58" y2="46"/><line x1="10" y1="54" x2="54" y2="54"/><line x1="14" y1="38" x2="50" y2="38"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Fog at night"><path d="M14 32 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#D0D4D8" stroke="#9DA5AD" stroke-width="1.5"/><g stroke="#9DA5AD" stroke-width="3" stroke-linecap="round"><line x1="6"  y1="46" x2="58" y2="46"/><line x1="10" y1="54" x2="54" y2="54"/><line x1="14" y1="38" x2="50" y2="38"/></g><g fill="#C4C4D4" opacity=".5"><circle cx="16" cy="20" r="1.5"/><circle cx="48" cy="16" r="1"/><circle cx="54" cy="26" r="1.2"/></g></svg>' },
            48: { desc: 'Rime fog',       icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Rime fog"><path d="M14 32 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="1.5"/><g stroke="#9DA5AD" stroke-width="3" stroke-linecap="round"><line x1="6"  y1="46" x2="58" y2="46"/><line x1="10" y1="54" x2="54" y2="54"/><line x1="14" y1="38" x2="50" y2="38"/></g><g fill="#9DA5AD"><circle cx="20" cy="58" r="1.5"/><circle cx="32" cy="60" r="1.5"/><circle cx="44" cy="58" r="1.5"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Rime fog at night"><path d="M14 32 q0 -8 8 -8 q3 -6 11 -6 q9 0 9 9 q5 0 5 6 q0 5 -6 5 h-22 q-5 0 -5 -6 z" fill="#D0D4D8" stroke="#9DA5AD" stroke-width="1.5"/><g stroke="#9DA5AD" stroke-width="3" stroke-linecap="round"><line x1="6"  y1="46" x2="58" y2="46"/><line x1="10" y1="54" x2="54" y2="54"/><line x1="14" y1="38" x2="50" y2="38"/></g><g fill="#9DA5AD"><circle cx="20" cy="58" r="1.5"/><circle cx="32" cy="60" r="1.5"/><circle cx="44" cy="58" r="1.5"/></g><g fill="#C4C4D4" opacity=".5"><circle cx="16" cy="20" r="1.5"/><circle cx="48" cy="16" r="1"/><circle cx="54" cy="26" r="1.2"/></g></svg>' },
            51: { desc: 'Light drizzle',  icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Light drizzle"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="#4A90E2"><circle cx="22" cy="50" r="2"/><circle cx="32" cy="54" r="2"/><circle cx="42" cy="50" r="2"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Light drizzle at night"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="#4A90E2" opacity=".8"><circle cx="22" cy="50" r="2"/><circle cx="32" cy="54" r="2"/><circle cx="42" cy="50" r="2"/></g></svg>' },
            53: { desc: 'Moderate drizzle', icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate drizzle"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="#4A90E2"><circle cx="20" cy="48" r="2.5"/><circle cx="32" cy="54" r="2.5"/><circle cx="44" cy="48" r="2.5"/><circle cx="26" cy="58" r="2"/><circle cx="38" cy="58" r="2"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate drizzle at night"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="#4A90E2" opacity=".8"><circle cx="20" cy="48" r="2.5"/><circle cx="32" cy="54" r="2.5"/><circle cx="44" cy="48" r="2.5"/><circle cx="26" cy="58" r="2"/><circle cx="38" cy="58" r="2"/></g></svg>' },
            55: { desc: 'Dense drizzle',  icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Dense drizzle"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#6B7480" stroke="#3F4750" stroke-width="2"/><g fill="#2C6FB8"><circle cx="18" cy="46" r="3"/><circle cx="28" cy="52" r="3"/><circle cx="38" cy="46" r="3"/><circle cx="46" cy="52" r="3"/><circle cx="22" cy="58" r="2.5"/><circle cx="34" cy="60" r="2.5"/><circle cx="42" cy="58" r="2.5"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Dense drizzle at night"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><g fill="#2C6FB8" opacity=".8"><circle cx="18" cy="46" r="3"/><circle cx="28" cy="52" r="3"/><circle cx="38" cy="46" r="3"/><circle cx="46" cy="52" r="3"/><circle cx="22" cy="58" r="2.5"/><circle cx="34" cy="60" r="2.5"/><circle cx="42" cy="58" r="2.5"/></g></svg>' },
            61: { desc: 'Slight rain',    icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Slight rain"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="none" stroke="#4A90E2" stroke-width="2.5" stroke-linecap="round"><path d="M22 48 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M32 52 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M42 48 q-2 4 0 6 q2 -2 0 -6 z"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Slight rain at night"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="none" stroke="#4A90E2" stroke-width="2.5" stroke-linecap="round" opacity=".8"><path d="M22 48 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M32 52 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M42 48 q-2 4 0 6 q2 -2 0 -6 z"/></g></svg>' },
            63: { desc: 'Moderate rain',  icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate rain"><path d="M14 30 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="none" stroke="#2C6FB8" stroke-width="3" stroke-linecap="round"><path d="M20 46 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M32 48 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M44 46 q-3 6 0 10 q3 -4 0 -10 z"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate rain at night"><path d="M14 30 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#5A626C" stroke="#3A4048" stroke-width="2"/><g fill="none" stroke="#2C6FB8" stroke-width="3" stroke-linecap="round" opacity=".8"><path d="M20 46 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M32 48 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M44 46 q-3 6 0 10 q3 -4 0 -10 z"/></g></svg>' },
            65: { desc: 'Heavy rain',     icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy rain"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><g fill="none" stroke="#1A4A82" stroke-width="3.5" stroke-linecap="round"><path d="M18 44 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M30 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M42 44 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M54 46 q-4 8 0 14 q4 -6 0 -14 z"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy rain at night"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#3A4048" stroke="#1A1D22" stroke-width="2"/><g fill="none" stroke="#1A4A82" stroke-width="3.5" stroke-linecap="round" opacity=".8"><path d="M18 44 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M30 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M42 44 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M54 46 q-4 8 0 14 q4 -6 0 -14 z"/></g></svg>' },
            71: { desc: 'Slight snow',    icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Slight snow"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#E8ECEF" stroke="#9DA5AD" stroke-width="2"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1"><circle cx="22" cy="50" r="3"/><circle cx="32" cy="54" r="3"/><circle cx="42" cy="50" r="3"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Slight snow at night"><path d="M14 32 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#C8CCD0" stroke="#9DA5AD" stroke-width="2"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1"><circle cx="22" cy="50" r="3"/><circle cx="32" cy="54" r="3"/><circle cx="42" cy="50" r="3"/></g></svg>' },
            73: { desc: 'Moderate snow',  icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate snow"><path d="M14 30 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#B8C0C8" stroke="#6B7480" stroke-width="2"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1.5"><circle cx="20" cy="48" r="4"/><circle cx="32" cy="54" r="4"/><circle cx="44" cy="48" r="4"/><circle cx="26" cy="58" r="3"/><circle cx="38" cy="58" r="3"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Moderate snow at night"><path d="M14 30 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#98A0A8" stroke="#6B7480" stroke-width="2"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1.5"><circle cx="20" cy="48" r="4"/><circle cx="32" cy="54" r="4"/><circle cx="44" cy="48" r="4"/><circle cx="26" cy="58" r="3"/><circle cx="38" cy="58" r="3"/></g></svg>' },
            75: { desc: 'Heavy snow',     icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy snow"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#6B7480" stroke="#3F4750" stroke-width="2"/><g fill="#FFFFFF" stroke="#1A4A82" stroke-width="1.5"><circle cx="18" cy="46" r="5"/><circle cx="30" cy="52" r="5"/><circle cx="42" cy="46" r="5"/><circle cx="50" cy="52" r="5"/><circle cx="24" cy="58" r="4"/><circle cx="40" cy="58" r="4"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy snow at night"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><g fill="#FFFFFF" stroke="#1A4A82" stroke-width="1.5"><circle cx="18" cy="46" r="5"/><circle cx="30" cy="52" r="5"/><circle cx="42" cy="46" r="5"/><circle cx="50" cy="52" r="5"/><circle cx="24" cy="58" r="4"/><circle cx="40" cy="58" r="4"/></g></svg>' },
            80: { desc: 'Rain showers',   icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Rain showers"><circle cx="20" cy="18" r="8" fill="#FFD23F"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#9DA5AD" stroke="#6B7480" stroke-width="2"/><g fill="none" stroke="#4A90E2" stroke-width="2.5" stroke-linecap="round"><path d="M22 48 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M32 52 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M42 48 q-2 4 0 6 q2 -2 0 -6 z"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Rain showers at night"><circle cx="20" cy="18" r="7" fill="#C4C4D4" opacity=".8"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="none" stroke="#4A90E2" stroke-width="2.5" stroke-linecap="round" opacity=".8"><path d="M22 48 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M32 52 q-2 4 0 6 q2 -2 0 -6 z"/><path d="M42 48 q-2 4 0 6 q2 -2 0 -6 z"/></g></svg>' },
            81: { desc: 'Heavy rain showers', icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy rain showers"><circle cx="20" cy="18" r="8" fill="#FFD23F"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#7A828C" stroke="#4A5058" stroke-width="2"/><g fill="none" stroke="#2C6FB8" stroke-width="3" stroke-linecap="round"><path d="M20 48 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M32 50 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M44 48 q-3 6 0 10 q3 -4 0 -10 z"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Heavy rain showers at night"><circle cx="20" cy="18" r="7" fill="#C4C4D4" opacity=".7"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#5A626C" stroke="#3A4048" stroke-width="2"/><g fill="none" stroke="#2C6FB8" stroke-width="3" stroke-linecap="round" opacity=".8"><path d="M20 48 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M32 50 q-3 6 0 10 q3 -4 0 -10 z"/><path d="M44 48 q-3 6 0 10 q3 -4 0 -10 z"/></g></svg>' },
            82: { desc: 'Violent rain showers', icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Violent rain showers"><circle cx="20" cy="18" r="8" fill="#FFD23F"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><g fill="none" stroke="#1A4A82" stroke-width="3.5" stroke-linecap="round"><path d="M18 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M30 48 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M42 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M52 48 q-4 8 0 14 q4 -6 0 -14 z"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Violent rain showers at night"><circle cx="20" cy="18" r="7" fill="#C4C4D4" opacity=".6"/><path d="M14 36 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#3A4048" stroke="#1A1D22" stroke-width="2"/><g fill="none" stroke="#1A4A82" stroke-width="3.5" stroke-linecap="round" opacity=".8"><path d="M18 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M30 48 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M42 46 q-4 8 0 14 q4 -6 0 -14 z"/><path d="M52 48 q-4 8 0 14 q4 -6 0 -14 z"/></g></svg>' },
            95: { desc: 'Thunderstorm',   icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Thunderstorm"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><path d="M30 38 L22 50 L30 50 L26 60 L40 44 L32 44 L36 38 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Thunderstorm at night"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#2C3038" stroke="#1A1D22" stroke-width="2"/><path d="M30 38 L22 50 L30 50 L26 60 L40 44 L32 44 L36 38 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/></svg>' },
            96: { desc: 'Thunderstorm with hail', icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Thunderstorm with hail"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#4A5058" stroke="#2C3038" stroke-width="2"/><path d="M30 38 L22 50 L30 50 L26 60 L40 44 L32 44 L36 38 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1"><circle cx="16" cy="56" r="2.5"/><circle cx="24" cy="60" r="2.5"/><circle cx="40" cy="60" r="2.5"/><circle cx="48" cy="56" r="2.5"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Thunderstorm with hail at night"><path d="M14 28 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#2C3038" stroke="#1A1D22" stroke-width="2"/><path d="M30 38 L22 50 L30 50 L26 60 L40 44 L32 44 L36 38 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/><g fill="#FFFFFF" stroke="#4A90E2" stroke-width="1"><circle cx="16" cy="56" r="2.5"/><circle cx="24" cy="60" r="2.5"/><circle cx="40" cy="60" r="2.5"/><circle cx="48" cy="56" r="2.5"/></g></svg>' },
            99: { desc: 'Severe thunderstorm', icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Severe thunderstorm"><path d="M14 26 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#2C3038" stroke="#1A1D22" stroke-width="2"/><path d="M30 36 L20 50 L28 50 L24 62 L42 42 L34 42 L38 36 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/><g fill="#FFFFFF" stroke="#1A4A82" stroke-width="1.5"><circle cx="14" cy="56" r="3"/><circle cx="22" cy="60" r="3"/><circle cx="38" cy="60" r="3"/><circle cx="50" cy="56" r="3"/></g></svg>', night_icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Severe thunderstorm at night"><path d="M14 26 q0 -10 10 -10 q4 -8 14 -8 q12 0 12 12 q6 0 6 8 q0 6 -8 6 h-28 q-6 0 -6 -8 z" fill="#1A1D22" stroke="#0D0F12" stroke-width="2"/><path d="M30 36 L20 50 L28 50 L24 62 L42 42 L34 42 L38 36 z" fill="#FFD23F" stroke="#B8860B" stroke-width="1.5" stroke-linejoin="round"/><g fill="#FFFFFF" stroke="#1A4A82" stroke-width="1.5"><circle cx="14" cy="56" r="3"/><circle cx="22" cy="60" r="3"/><circle cx="38" cy="60" r="3"/><circle cx="50" cy="56" r="3"/></g></svg>' }
        };

        function getWmo(code, isNight) {
            if (typeof ripleyWeatherIconSvg === 'function') {
                return { desc: ripleyWeatherIconDescription(code), icon: ripleyWeatherIconSvg(code, isNight) };
            }
            const entry = WMO_CODES[code] || null;
            if (!entry) return { desc: 'Unknown', icon: '<svg viewBox="0 0 64 64" xmlns="http://www.w3.org/2000/svg" aria-label="Unknown"><circle cx="32" cy="32" r="20" fill="none" stroke="#9DA5AD" stroke-width="3"/><text x="32" y="40" text-anchor="middle" font-size="24" fill="#9DA5AD" font-family="sans-serif">?</text></svg>' };
            return { desc: entry.desc, icon: (isNight && entry.night_icon) ? entry.night_icon : entry.icon };
        }

        // State management
        let state = {
            data: null,
            location: '',
            unit: 'C',
            color: '#ffffff',
            accent: '#e94560',
            bgColor: '#ffffff',
            bgOpacity: 0.05,
            textOutlineColor: '#000000',
            textOutlineWidth: 0,
            lat: 0,
            lon: 0,
            shownDays: 4,
            weatherOffset: 0,
            customIcons: {},
            compact: false,
            rowHeight: 8,
            morningSplit: false
        };

        function colorToRgba(color, opacity) {
            const a = Math.max(0, Math.min(1, Number(opacity)));
            const m = /^#?([a-f\d]{2})([a-f\d]{2})([a-f\d]{2})$/i.exec(color || '');
            if (!m) return `rgba(255,255,255,${a})`;
            return `rgba(${parseInt(m[1],16)},${parseInt(m[2],16)},${parseInt(m[3],16)},${a})`;
        }

        function escapeAttr(s) {
            return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
        }

        function convertTemp(c, unit) {
            if (unit === 'F') {
                return Math.round((c * 9/5) + 32);
            }
            return Math.round(c);
        }

        async function init() {
            const params = new URLSearchParams(window.location.search);
            const requestedFont = params.get('font_family');
            if (requestedFont && requestedFont.length <= 200 && !/[<>]|javascript:/i.test(requestedFont)) document.documentElement.style.fontFamily = requestedFont;
            state.lat = parseFloat(params.get('lat'));
            state.lon = parseFloat(params.get('lon'));
            state.template = params.get('template') || '3day';
            state.color = params.get('color') || '#ffffff';
            state.accent = params.get('accent') || '#e94560';
            state.bgColor = params.get('bg_color') || params.get('box_color') || '#ffffff';
            state.bgOpacity = Number(params.get('bg_opacity') ?? params.get('box_opacity') ?? 0.05);
            if (!Number.isFinite(state.bgOpacity)) state.bgOpacity = 0.05;
            state.textOutlineColor = params.get('text_outline_color') || '#000000';
            state.textOutlineWidth = Number(params.get('text_outline_width') ?? 0);
            if (!Number.isFinite(state.textOutlineWidth)) state.textOutlineWidth = 0;
            state.location = params.get('location') || '';
            state.unit = (params.get('unit') || 'C').toUpperCase();
            state.compact = params.get('compact') === 'true';
            const rhParam = parseFloat(params.get('row_height'));
            state.rowHeight = (Number.isFinite(rhParam) && rhParam > 0) ? rhParam : 8;
            const shownDaysParam = parseInt(params.get('shown_days'), 10);
            if (Number.isFinite(shownDaysParam) && shownDaysParam >= 1) {
                state.shownDays = Math.min(shownDaysParam, 7);
            } else {
                state.shownDays = 4; // default
            }
            const weatherOffsetParam = parseInt(params.get('weather_offset'), 10);
            state.weatherOffset = (Number.isFinite(weatherOffsetParam) && weatherOffsetParam >= 0) ? Math.min(weatherOffsetParam, 6) : 0;
            state.morningSplit = params.get('morning_split') === 'true';

            // Read custom icons map from URL param (JSON-encoded)
            try {
                const iconsParam = params.get('icons');
                if (iconsParam) state.customIcons = JSON.parse(iconsParam) || {};
            } catch (e) { /* ignore parse errors */ }

            if (isNaN(state.lat) || isNaN(state.lon)) {
                showError('Invalid location coordinates.');
                return;
            }

            document.documentElement.style.setProperty('--text-color', state.color);
            document.documentElement.style.setProperty('--accent-color', state.accent);
            document.documentElement.style.setProperty('--forecast-bg', colorToRgba(state.bgColor, state.bgOpacity));
            document.documentElement.style.setProperty('--text-outline-color', state.textOutlineColor);
            document.documentElement.style.setProperty('--text-outline-width', `${Math.max(0, state.textOutlineWidth)}px`);

            // If lat/lon is exactly 0/0, use sample data (player default or for previews)
            if (state.lat === 0 && state.lon === 0) {
                state.data = getSampleWeatherData();
                state.location = state.location || 'Sample Location';
                render();
                return;
            }

            try {
                const response = await fetch(`/api/weather/forecast?lat=${encodeURIComponent(state.lat)}&lon=${encodeURIComponent(state.lon)}`, {cache: 'no-store'});
                if (!response.ok) throw new Error('Network response was not ok');
                state.data = await response.json();
                render();
            } catch (error) {
                console.error(error);
                if (state.data && state.data.source !== 'sample') {
                    state.data.weather_meta = Object.assign({}, state.data.weather_meta, {
                        stale: true,
                        stale_reason: 'Weather provider unavailable; showing last successful reading'
                    });
                    render();
                } else {
                    showError('Weather data is unavailable.');
                }
            }
        }

        // Returns a local sample dataset for previews and missing coordinates.
        function getSampleWeatherData() {
            const today = new Date();
            const dates = [];
            for (let i = 0; i < 7; i++) {
                const d = new Date(today);
                d.setDate(today.getDate() + i);
                dates.push(d.toISOString().slice(0, 10));
            }
            return {
                current_weather: { temperature: 22, weathercode: 2, windspeed: 8, winddirection: 180, is_daytime: true },
                daily: {
                    time: dates,
                    weathercode: [2, 3, 61, 80, 95, 71, 1],
                    weather_code_day: [2, 3, 61, 80, 95, 71, 1],
                    weather_code_night: [3, 3, 63, 3, 95, 73, 3],
                    weather_code_morning: [45, null, null, null, null, null, null],
                    weather_morning_until: [`${dates[0]}T08:00:00-04:00`, null, null, null, null, null, null],
                    temperature_2m_max: [24, 22, 18, 19, 16, 5, 8],
                    temperature_2m_min: [15, 14, 12, 11, 10, -2, 0],
                    precipitation_probability_max: [20, 35, 70, 55, 85, 60, 10]
                }
            };
        }

        function formatMorningUntil(value) {
            if (!value) return '';
            const d = new Date(value);
            if (!Number.isFinite(d.getTime())) return '';
            try { return new Intl.DateTimeFormat(undefined, {hour: 'numeric', minute: '2-digit', timeZone: state.data?.timezone || undefined}).format(d); }
            catch (_) { return d.toLocaleTimeString(undefined, {hour: 'numeric', minute: '2-digit'}); }
        }

        function morningSplitIcon(morningCode, dayCode, untilText, dataIdx) {
            const morningKey = String(morningCode);
            const dayKey = String(dayCode);
            const leftUrl = state.customIcons && (state.customIcons[morningKey] || state.customIcons[`${morningKey}_day`]);
            const rightUrl = state.customIcons && (state.customIcons[dayKey] || state.customIcons[`${dayKey}_day`]);
            const left = leftUrl ? `<img src="${escapeAttr(leftUrl)}" alt="${escapeAttr(getWmo(morningCode, false).desc)}"/>` : getWmo(morningCode, false).icon;
            const right = rightUrl ? `<img src="${escapeAttr(rightUrl)}" alt="${escapeAttr(getWmo(dayCode, false).desc)}"/>` : getWmo(dayCode, false).icon;
            const label = untilText ? `${getWmo(morningCode, false).desc} until ${untilText}; then ${getWmo(dayCode, false).desc}` : `${getWmo(morningCode, false).desc}, then ${getWmo(dayCode, false).desc}`;
            return `<div class="morning-split-icon" title="${escapeAttr(label)}"><div>${left}</div><span aria-hidden="true">→</span><div>${right}</div></div>`;
        }

        function showError(msg) {
            const app = document.getElementById('app');
            app.replaceChildren(Object.assign(document.createElement('div'), {
                className: 'error',
                textContent: String(msg || 'Weather unavailable'),
            }));
        }

        function render() {
            if (!state.data) return;
            const app = document.getElementById('app');
            app.innerHTML = '';

            if (state.compact) {
                app.classList.add('compact');
                document.documentElement.style.setProperty('--row-height', state.rowHeight + 'vh');
            } else {
                app.classList.remove('compact');
            }

            // Render header (skipped in compact mode)
            if (!state.compact) {
                const header = document.createElement('div');
                header.className = 'header';

                const locEl = document.createElement('div');
                locEl.className = 'location';
                locEl.textContent = state.location || 'Current Location';
                header.appendChild(locEl);

                const curTempEl = document.createElement('div');
                curTempEl.className = 'current-temp';
                const currentTempC = state.data.current_weather.temperature;
                const displayTemp = convertTemp(currentTempC, state.unit);
                const unitSymbol = state.unit === 'F' ? '°F' : '°C';
                curTempEl.textContent = `${displayTemp}${unitSymbol}`;
                header.appendChild(curTempEl);

                app.appendChild(header);
            }

            const forecastContainer = document.createElement('div');
            forecastContainer.className = 'forecast-container';

            for (let i = 0; i < state.shownDays; i++) {
                const dataIdx = state.weatherOffset + i;
                const item = document.createElement('div');
                item.className = 'forecast-item';

                const dateObj = new Date(state.data.daily.time[dataIdx] + 'T00:00:00Z');
                let dayLabel;

                if (state.weatherOffset === 0 && i === 0) {
                    dayLabel = 'Today';
                } else {
                    // The API date is already a local calendar date. Format it
                    // in UTC so the browser's timezone cannot shift it backward.
                    dayLabel = dateObj.toLocaleDateString(undefined, { weekday: 'short', timeZone: 'UTC' });
                }

                const dayCodes = state.data.daily.weather_code_day || state.data.daily.weathercode_day || state.data.daily.weather_code || state.data.daily.weathercode;
                const nightCodes = state.data.daily.weather_code_night || state.data.daily.weathercode_night || dayCodes;
                const isNight = dataIdx === 0 && state.data.current_weather && state.data.current_weather.is_daytime === false;
                const forecastCode = (isNight ? nightCodes : dayCodes)[dataIdx];
                const wmoCode = dataIdx === 0 && state.data.current_weather && state.data.current_weather.weathercode != null
                    ? state.data.current_weather.weathercode
                    : forecastCode;
                const wmo = getWmo(wmoCode, isNight);
                // Use a matching custom day/night icon when one is configured.
                const morningCodes = state.data.daily.weather_code_morning || state.data.daily.weathercode_morning || [];
                const morningUntilSeries = state.data.daily.weather_morning_until || state.data.daily.morning_weather_until || [];
                const morningCode = morningCodes[dataIdx];
                const morningUntilText = formatMorningUntil(morningUntilSeries[dataIdx]);
                const hasMorningTransition = morningCode != null && String(morningCode) !== String(forecastCode);
                const iconKey = `${wmoCode}${isNight ? '_night' : ''}`;
                const customIconUrl = state.customIcons && (state.customIcons[iconKey] || state.customIcons[String(wmoCode)]);
                let iconHtml = customIconUrl
                    ? `<img src="${escapeAttr(customIconUrl)}" alt="${escapeAttr(wmo.desc)}" style="width:100%; height:100%; object-fit:contain;"/>`
                    : wmo.icon;
                if (state.morningSplit && !isNight && hasMorningTransition) {
                    iconHtml = morningSplitIcon(morningCode, forecastCode, morningUntilText, dataIdx);
                }
                const maxTempC = state.data.daily.temperature_2m_max[dataIdx];

                const minTempC = state.data.daily.temperature_2m_min[dataIdx];


                const maxTemp = convertTemp(maxTempC, state.unit);
                const minTemp = convertTemp(minTempC, state.unit);

                const unitSymbol = state.unit === 'F' ? '°F' : '°C';
                item.innerHTML = `
                    <div class="day">${dayLabel}</div>
                    <div class="icon">${iconHtml}</div>
                    <div class="temps">
                        <div class="temp-max">${maxTemp}${unitSymbol}</div>
                        <div class="temp-min">${minTemp}${unitSymbol}</div>
                    </div>
                `;

                forecastContainer.appendChild(item);
            }

            app.appendChild(forecastContainer);
        }

        init();
    
