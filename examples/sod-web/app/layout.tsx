import type { ReactNode } from "react";

export const metadata = {
  title: "sod-web",
  description: "Three bogs, one board: offline-first emoji reactions on sod",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body style={{ margin: 0, fontFamily: "ui-sans-serif, system-ui, sans-serif" }}>
        {children}
      </body>
    </html>
  );
}
