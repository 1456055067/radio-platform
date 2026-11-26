import Link, { type LinkProps } from '@cloudscape-design/components/link';
import { useNavigate } from 'react-router';

/** Cloudscape link that navigates client-side. */
export function RLink({ href, children, ...rest }: LinkProps & { href: string }) {
  const navigate = useNavigate();
  return (
    <Link
      {...rest}
      href={href}
      onFollow={(e) => {
        e.preventDefault();
        navigate(href);
      }}
    >
      {children}
    </Link>
  );
}
