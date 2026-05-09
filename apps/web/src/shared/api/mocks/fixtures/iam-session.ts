import type { GetIamSessionResponse } from "../../generated/types.gen";

type IamSessionFixture = GetIamSessionResponse;

const DEFAULT_IAM_SESSION = {
  expiresAt: "2026-05-05T15:30:00.000Z",
  sessionId: "session-demo-admin",
  user: {
    displayName: "Demo Admin",
    email: "admin@example.test",
    login: "admin",
    principalId: "principal-demo-admin",
  },
} satisfies IamSessionFixture;

export default function iamSession(overrides: Partial<IamSessionFixture> = {}): IamSessionFixture {
  const { user, ...sessionOverrides } = overrides;

  return {
    ...DEFAULT_IAM_SESSION,
    ...sessionOverrides,
    user: {
      ...DEFAULT_IAM_SESSION.user,
      ...(user ?? {}),
    },
  };
}
