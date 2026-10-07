/** Keep the approved X artwork and select a background suited to the system theme. */
export function Logo() {
  return (
    <picture className="brand-logo">
      <source media="(prefers-color-scheme: dark)" srcSet="/icon-dark.svg" />
      <img src="/icon.png" alt="" />
    </picture>
  );
}
