import type { ReactNode } from "react";
import "./globals.css";

export const metadata = {
  title: "sod-web",
  description: "Three bogs, one board: offline-first emoji reactions on sod",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
