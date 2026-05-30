# CLAUDE.md - Project Styling & UX/UI Guidelines

This file outlines the premium, elegant, and compact UX/UI guidelines for this project.

## UX/UI Design Principles (Premium, Minimalist & Compact)

Always follow these guidelines when generating, editing, or refactoring front-end code and user interfaces:

### 1. Color Palette (โทนสีเรียบหรู)
- **Neutral & Monochromatic**: Stick strictly to a limited neutral palette (whites, deep charcoals, slate grays, and soft off-whites). Avoid vibrant colors.
- **Accents**: Use a single premium primary accent tone (e.g., deep royal blue, sapphire blue, or cobalt blue `#0055ff` / `#0066cc`) extremely sparingly (less than 5% of the UI) for primary actions, active states, or subtle focus points. Avoid yellow, gold, or warm accent colors.
- **Gradients**: Use soft, low-intensity dark-to-light gradients (e.g., `#121212` to `#1e1e1e`) instead of flat bright colors.

### 2. High-Density Compact Spacing (ชิดกระชับ ไม่ห่างกันมาก)
- **Tight Layouts**: Maintain a high-density, cohesive look. Keep paddings and margins compact to make the interface look professional and structured.
- **Rules**:
  - Prefer `gap: 8px` or `12px` over large default gaps.
  - Keep margins and padding focused (`padding: 8px 12px` or similar).
  - Structure elements cleanly with borders instead of large whitespace gaps.

### 3. Borders & Corners (กรอบและขอบคมชัด)
- **Radius**: Use a small, sharp border-radius (4px to 8px max). Do not use extremely rounded or circular bubbles.
- **Borders**: Separate sections with extremely thin, low-opacity borders (e.g., `1px solid rgba(255, 255, 255, 0.08)` for dark mode, or `rgba(0, 0, 0, 0.06)` for light mode).

### 4. Typography (การจัดวางฟอนต์)
- **Hierarchy**: Establish hierarchy using weight variations (e.g., Thin/Light/Medium/Semi-bold) and size contrast rather than relying on color.
- **Spacing**: Slightly increase letter-spacing (e.g., `letter-spacing: 0.05em`) for uppercase headings to enhance readability and premium feel.

### 5. Details (ลูกเล่นความหรูหรา)
- **Glassmorphism**: Use backdrop-blur (`backdrop-filter: blur(10px)`) and semi-transparent backgrounds where appropriate.
- **Shadows**: Use soft, low-intensity diffused shadows.
- **Animations**: Implement subtle micro-interactions on hover/active states (e.g., transition opacity by 0.15s, or slight translate Y by -1px).

---

## Technical Setup Reference (CSS Variables)

Use these variables to enforce consistent design tokens:

```css
:root {
  --bg-primary: #0a0a0a;
  --bg-secondary: #121212;
  --bg-tertiary: #1a1a1a;
  
  --border-subtle: rgba(255, 255, 255, 0.08);
  --border-focus: rgba(255, 255, 255, 0.2);
  
  --text-primary: #f5f5f7;
  --text-secondary: #8e8e93;
  --text-muted: #48484a;
  
  --accent-color: #0066cc; /* สีน้ำเงินพรีเมียม (Royal/Sapphire Blue) */
  
  --space-xxs: 4px;
  --space-xs: 8px;
  --space-sm: 12px;
  --space-md: 16px;
  
  --radius-sm: 4px;
  --radius-md: 6px;
  
  --shadow-premium: 0 4px 20px rgba(0, 0, 0, 0.5), 0 1px 2px rgba(255, 255, 255, 0.05);
}
```
