import React, { useEffect, useState } from 'react';
import { ThemeMode } from 'shared/types';
import { ThemeProviderContext } from '@/shared/hooks/useTheme';

type ThemeProviderProps = {
  children: React.ReactNode;
  initialTheme?: ThemeMode;
};

export function ThemeProvider({
  children,
  initialTheme = ThemeMode.SYSTEM,
  ...props
}: ThemeProviderProps) {
  const [theme, setThemeState] = useState<ThemeMode>(initialTheme);

  // Update theme when initialTheme changes
  useEffect(() => {
    setThemeState(initialTheme);
  }, [initialTheme]);

  useEffect(() => {
    const root = window.document.documentElement;

    const applyTheme = (resolvedTheme: 'light' | 'dark') => {
      root.classList.remove('light', 'dark');
      root.classList.add(resolvedTheme);
    };

    if (theme === ThemeMode.SYSTEM) {
      const query = window.matchMedia('(prefers-color-scheme: dark)');
      const applySystemTheme = () => {
        applyTheme(query.matches ? 'dark' : 'light');
      };
      const handleSystemThemeChange = (event: MediaQueryListEvent) => {
        applyTheme(event.matches ? 'dark' : 'light');
      };

      applySystemTheme();
      query.addEventListener('change', handleSystemThemeChange);
      document.addEventListener('visibilitychange', applySystemTheme);

      return () => {
        query.removeEventListener('change', handleSystemThemeChange);
        document.removeEventListener('visibilitychange', applySystemTheme);
      };
    }

    applyTheme(theme.toLowerCase() as 'light' | 'dark');
  }, [theme]);

  const setTheme = (newTheme: ThemeMode) => {
    setThemeState(newTheme);
  };

  const value = {
    theme,
    setTheme,
  };

  return (
    <ThemeProviderContext.Provider {...props} value={value}>
      {children}
    </ThemeProviderContext.Provider>
  );
}
