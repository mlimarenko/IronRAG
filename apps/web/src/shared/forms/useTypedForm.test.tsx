import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { z } from "zod";

import { FormInputField } from "./FormField";
import { nonEmptyString } from "./formSchemas";
import { useTypedForm, type TypedFormMutation } from "./useTypedForm";

const schema = z.object({
  name: nonEmptyString("Name is required"),
});

type TestValues = z.output<typeof schema>;

function TestForm({ mutation }: { mutation: TypedFormMutation<TestValues> }) {
  const form = useTypedForm({
    schema,
    defaultValues: { name: "" },
  });
  const submit = form.submitWithMutation(mutation);

  return (
    <div>
      <FormInputField
        formState={form.formState}
        label="Name"
        name="name"
        registration={form.register("name")}
      />
      <button type="button" onClick={() => void submit()}>
        Submit
      </button>
    </div>
  );
}

describe("useTypedForm", () => {
  it("does not submit when zod validation fails", async () => {
    const mutation = { mutateAsync: vi.fn<() => Promise<void>>() };
    render(<TestForm mutation={mutation} />);

    fireEvent.click(screen.getByText("Submit"));

    await waitFor(() => {
      expect(screen.getByRole("alert")).toHaveTextContent("Name is required");
    });
    expect(mutation.mutateAsync).not.toHaveBeenCalled();
  });

  it("calls the mutation when validation passes", async () => {
    const mutation = { mutateAsync: vi.fn(async () => undefined) };
    render(<TestForm mutation={mutation} />);

    fireEvent.change(screen.getByLabelText("Name"), {
      target: { value: "Alpha" },
    });
    fireEvent.click(screen.getByText("Submit"));

    await waitFor(() => {
      expect(mutation.mutateAsync).toHaveBeenCalledWith({ name: "Alpha" });
    });
  });
});
