FROM python:3.13-slim
RUN pip install --no-cache-dir cryptography==44.0.2
WORKDIR /app
COPY app.py index.html /app/
USER 65534:65534
EXPOSE 8080
CMD ["python", "-u", "/app/app.py"]
