import darkWordmark from "../../../assets/logos/xrun-wordmark-dark.svg";
import lightWordmark from "../../../assets/logos/xrun-wordmark-light.svg";

/** Use the canonical wordmark with lettering suited to the system theme. */
export function Logo() {
  return (
    <picture className="brand-logo">
      <source media="(prefers-color-scheme: dark)" srcSet={darkWordmark} />
      <img src={lightWordmark} alt="xrun" width={406} height={132} />
    </picture>
  );
}
