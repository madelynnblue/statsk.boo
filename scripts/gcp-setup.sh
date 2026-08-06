#!/bin/bash
# GCP setup for Cloud Run deployment via GitHub Actions.
# Idempotent — safe to run repeatedly.
# Usage: bash scripts/gcp-setup.sh <PROJECT_ID> <GITHUB_USER> <GITHUB_REPO>

set -euo pipefail

PROJECT_ID="${1:?missing PROJECT_ID}"
GITHUB_USER="${2:?missing GITHUB_USER}"
GITHUB_REPO="${3:?missing GITHUB_REPO}"

PROJECT_NUMBER=$(gcloud projects describe "$PROJECT_ID" --format='value(projectNumber)')
REGION="us-central1"
SA="github-actions"
SA_FULL="$SA@$PROJECT_ID.iam.gserviceaccount.com"

echo "=== Enabling APIs ==="
gcloud services enable \
    artifactregistry.googleapis.com \
    run.googleapis.com \
    iamcredentials.googleapis.com \
    --project="$PROJECT_ID"

echo "=== Creating Artifact Registry repo (if missing) ==="
if ! gcloud artifacts repositories describe docker --location="$REGION" --project="$PROJECT_ID" &>/dev/null; then
    gcloud artifacts repositories create docker \
        --repository-format=docker \
        --location="$REGION" \
        --project="$PROJECT_ID"
fi

echo "=== Creating service account: $SA (if missing) ==="
if ! gcloud iam service-accounts describe "$SA_FULL" --project="$PROJECT_ID" &>/dev/null; then
    gcloud iam service-accounts create "$SA" --project="$PROJECT_ID"
fi

echo "=== Granting IAM roles to $SA ==="
gcloud projects add-iam-policy-binding "$PROJECT_ID" \
    --member="serviceAccount:$SA_FULL" \
    --role="roles/artifactregistry.writer" \
    --condition=None

gcloud projects add-iam-policy-binding "$PROJECT_ID" \
    --member="serviceAccount:$SA_FULL" \
    --role="roles/run.admin" \
    --condition=None

gcloud projects add-iam-policy-binding "$PROJECT_ID" \
    --member="serviceAccount:$SA_FULL" \
    --role="roles/iam.serviceAccountUser" \
    --condition=None

echo "=== Setting up Workload Identity Federation ==="
if ! gcloud iam workload-identity-pools describe github --location=global --project="$PROJECT_ID" &>/dev/null; then
    gcloud iam workload-identity-pools create github \
        --location=global \
        --project="$PROJECT_ID"
fi

if ! gcloud iam workload-identity-pools providers describe github --location=global --workload-identity-pool=github --project="$PROJECT_ID" &>/dev/null; then
    gcloud iam workload-identity-pools providers create-oidc github \
        --location=global \
        --workload-identity-pool=github \
        --issuer-uri="https://token.actions.githubusercontent.com" \
        --attribute-mapping="google.subject=assertion.sub,attribute.actor=assertion.actor,attribute.repository=assertion.repository" \
        --attribute-condition="assertion.repository=='$GITHUB_USER/$GITHUB_REPO'" \
        --project="$PROJECT_ID"
fi

gcloud iam service-accounts add-iam-policy-binding "$SA_FULL" \
    --role="roles/iam.workloadIdentityUser" \
    --member="principalSet://iam.googleapis.com/projects/$PROJECT_NUMBER/locations/global/workloadIdentityPools/github/attribute.repository/$GITHUB_USER/$GITHUB_REPO" \
    --condition=None

REPO="$GITHUB_USER/$GITHUB_REPO"
WIF_PROVIDER_VALUE="projects/$PROJECT_NUMBER/locations/global/workloadIdentityPools/github/providers/github"

echo ""
echo "=== Setting GitHub Actions secrets on $REPO ==="

gh secret set WIF_PROVIDER        --body "$WIF_PROVIDER_VALUE" --repo "$REPO"
gh secret set WIF_SERVICE_ACCOUNT --body "$SA_FULL"            --repo "$REPO"
gh secret set GCP_PROJECT_ID      --body "$PROJECT_ID"         --repo "$REPO"

printf "DATABASE_URL (input hidden): "
read -r -s DATABASE_URL
echo
gh secret set DATABASE_URL --body "$DATABASE_URL" --repo "$REPO"

# The secret holds the JSON *contents*, not a local path: the app opens
# GOOGLE_SERVICE_ACCOUNT_PATH as a file path inside the container. The
# deployment must materialize the secret to a file (Cloud Run secret volume,
# or an entrypoint step writing it to e.g. /data/sa.json) and set
# GOOGLE_SERVICE_ACCOUNT_PATH to that path.
printf 'Path to downloaded service account JSON (e.g. ~/Downloads/wftda-sa.json): '
read -r sa_json_path
[ -f "$sa_json_path" ] || { echo "file not found: $sa_json_path" >&2; exit 1; }
gh secret set GOOGLE_SERVICE_ACCOUNT_PATH < "$sa_json_path"

echo "=== All secrets set. ==="
